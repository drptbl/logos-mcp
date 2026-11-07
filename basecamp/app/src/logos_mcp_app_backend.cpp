#include "logos_mcp_app_backend.h"

#include <QJsonDocument>
#include <QJsonObject>

#include "logos_sdk.h"

namespace {
constexpr int kPollMs = 2000;
constexpr int kWalletPollMs = 10000;
constexpr int kFirstPollMs = 250;
constexpr int kKeep = 200;

// logos_mcp answers every method with a JSON object. Anything else (an empty
// reply when the call itself failed) becomes an error, never a silent success.
QString checked(const QString& reply)
{
    if (QJsonDocument::fromJson(reply.toUtf8()).isObject())
        return reply;
    return QStringLiteral(R"({"error":"logos_mcp did not answer"})");
}

bool isError(const QString& reply)
{
    return QJsonDocument::fromJson(reply.toUtf8()).object().contains(QStringLiteral("error"));
}
}

QString LogosMcpAppBackend::token()
{
    return checked(modules().logos_mcp.token());
}

QString LogosMcpAppBackend::rotateToken()
{
    return checked(modules().logos_mcp.rotate_token());
}

QString LogosMcpAppBackend::setPermission(QString group, bool enabled)
{
    const QString reply = checked(modules().logos_mcp.set_permission(group, enabled));
    if (!isError(reply))
        setPermissions(reply);
    return reply;
}

QString LogosMcpAppBackend::setAgentAccount(QString accountId, bool agent)
{
    const QString reply = checked(modules().logos_mcp.set_agent_account(accountId, agent));
    // After replying: reading every balance first could make a saved change time out.
    QTimer::singleShot(0, [this]() { refreshWallet(); });
    return reply;
}

QString LogosMcpAppBackend::newAgentAccount()
{
    const QString reply = checked(modules().logos_mcp.new_agent_account());
    QTimer::singleShot(0, [this]() { refreshWallet(); });
    return reply;
}

QString LogosMcpAppBackend::approve(QString requestId)
{
    const QString reply = checked(modules().logos_mcp.approve(requestId));
    setApprovals(checked(modules().logos_mcp.approvals()));
    return reply;
}

QString LogosMcpAppBackend::deny(QString requestId)
{
    const QString reply = checked(modules().logos_mcp.deny(requestId));
    setApprovals(checked(modules().logos_mcp.approvals()));
    return reply;
}

void LogosMcpAppBackend::refreshWallet()
{
    setWallet(checked(modules().logos_mcp.wallet()));
}

void LogosMcpAppBackend::onContextReady()
{
    QObject::connect(&m_poll, &QTimer::timeout, [this]() { refresh(); });
    m_poll.start(kPollMs);
    QObject::connect(&m_walletPoll, &QTimer::timeout, [this]() { refreshWallet(); });
    m_walletPoll.start(kWalletPollMs);
    // Not from inside the hook: on Basecamp 0.4.0-rc.2 calls made this early are
    // refused because the backend's token isn't registered yet.
    QTimer::singleShot(kFirstPollMs, [this]() {
        refresh();
        refreshWallet();
    });
}

void LogosMcpAppBackend::refresh()
{
    setStatus(checked(modules().logos_mcp.status()));
    const QString permissions = checked(modules().logos_mcp.permissions());
    if (!isError(permissions))
        setPermissions(permissions);
    // Every poll: a waiting agent gives up after a minute.
    setApprovals(checked(modules().logos_mcp.approvals()));

    const QString reply = checked(modules().logos_mcp.activity(m_after));
    if (isError(reply))
        return;
    const QJsonObject page = QJsonDocument::fromJson(reply.toUtf8()).object();
    const qlonglong next = page.value(QStringLiteral("next")).toInteger(m_after);
    const QJsonArray entries = page.value(QStringLiteral("entries")).toArray();
    // The log started over (its file was reset): drop what came before.
    const bool reset = next < m_after;
    if (reset)
        m_activity = QJsonArray();
    m_after = next;
    if (entries.isEmpty() && !reset)
        return;
    for (const auto& entry : entries)
        m_activity.append(entry);
    while (m_activity.size() > kKeep)
        m_activity.removeFirst();
    setActivity(QString::fromUtf8(QJsonDocument(m_activity).toJson(QJsonDocument::Compact)));
}
