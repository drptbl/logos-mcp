#pragma once

#include <QJsonArray>
#include <QTimer>

#include "logos_module_context.h"
#include "rep_logos_mcp_app_source.h"

// The Agent app's backend: a thin relay to the logos_mcp module, which holds all
// of the state. It polls logos_mcp and feeds the .rep PROPs, and forwards the
// view's actions.
class LogosMcpAppBackend : public LogosMcpAppSimpleSource, public LogosModuleContext
{
public:
    QString token() override;
    QString rotateToken() override;
    QString setPermission(QString group, bool enabled) override;

    void onContextReady() override;

private:
    void refresh();

    QTimer m_poll;
    // Newest calls last, at most kKeep of them.
    QJsonArray m_activity;
    qlonglong m_after = 0;
};
