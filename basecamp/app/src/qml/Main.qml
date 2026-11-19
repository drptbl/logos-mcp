import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Logos.Theme
import Logos.Controls

// The Agent Access app: connect an MCP client, choose the tool groups the agent may
// use, and see its calls. All state lives in the logos_mcp module; the backend
// relays it as JSON PROPs.
Item {
    id: root

    readonly property var backend: logos.module("logos_mcp_app")
    property bool ready: false

    readonly property var status: parse(backend ? backend.status : "{}", {})
    readonly property var permissions: parse(backend ? backend.permissions : "{}", {})
    readonly property var activity: parse(backend ? backend.activity : "[]", [])
    readonly property var wallet: parse(backend ? backend.wallet : "{}", {})
    readonly property var approvals: parse(backend ? backend.approvals : "{}", {})
    readonly property var approvalPolicy: parse(backend ? backend.approvalPolicy : "{}", {})
    readonly property var seat: parse(backend ? backend.seat : "{}", {})

    // The approval policy being edited: loaded from what logos_mcp saved,
    // until the user changes something, and saved as a whole.
    property bool draftDirty: false
    property string agentLimit: "0"
    property int agentUpTo: 0
    property int agentAbove: 0
    property string nodeLimit: "0"
    property int nodeUpTo: 1
    property int nodeAbove: 1
    property string roomTopic: ""
    property var roomApprovers: []
    property int roomApprovals: 1
    property int roomExpires: 60
    // The chosen room's members, as the agent's seat sees them.
    property var members: []
    property bool settingUpSeat: false
    // Room approval is on once the agent's seat is set up: until then the
    // tiers offer Send and Ask me only, and nothing asks Muster anything.
    readonly property bool roomOn: !!root.seat.set_up
    readonly property var approverKeys: ["send", "ask", "room"]
    readonly property var approverNames: [qsTr("Send"), qsTr("Ask me"), qsTr("Room")]
    // The account waiting for the user to confirm handing it over.
    property var handOver: null

    property string token: ""
    property bool tokenShown: false
    // Accounts made to be paid for one service job stay folded into one line.
    property bool showJobAccounts: false
    readonly property var jobAccounts: (wallet.accounts || []).filter(function (a) { return a.job })
    readonly property int waitingCount: (root.approvals.waiting || []).length + (root.approvals.room || []).length

    // The content column: as wide as reads well, centred in the view.
    readonly property int contentWidth: Math.min(width - 2 * Theme.spacing.xlarge, 820)

    readonly property var groups: [
        { key: "messaging", title: qsTr("Messaging"), detail: qsTr("Read and send messages, RLN status") },
        { key: "storage", title: qsTr("Storage"), detail: qsTr("Upload and download files in the files folder, and remove what the agent stored") },
        { key: "wallet", title: qsTr("Wallet"), detail: qsTr("See accounts and balances, create agent accounts") },
        { key: "transfers", title: qsTr("Transfers"), detail: qsTr("Send LEZ from the agent accounts, and ask to send TestToken from your chain node") },
        { key: "discovery", title: qsTr("Discovery"), detail: qsTr("Advertise and find services, see connected peers") },
        { key: "chain", title: qsTr("Chain"), detail: qsTr("Read the Logos chain, and claim your node's leader rewards, which costs a fee") },
        { key: "chat", title: qsTr("Chat"), detail: qsTr("Uses your Basecamp chat account: the agent reads and sends in every Logos Chat conversation you have") }
    ]

    function parse(text, fallback) {
        try { return JSON.parse(text) } catch (e) { return fallback }
    }

    // A confirmation that closes itself.
    function say(message) {
        errorNotice.shown = false
        toast.show("", message)
    }

    // An error stays until closed, or until the view changes.
    function fail(message) {
        toast.hide()
        errorNotice.message = message
        errorNotice.shown = true
    }

    // `failed`, if given, runs after an error is shown.
    function call(pending, done, failed) {
        logos.watch(pending, function (raw) {
            var reply = parse(raw, {})
            if (reply.error) {
                root.fail(reply.error)
                if (failed) failed()
            } else done(reply)
        }, function (error) {
            root.fail(String(error))
            if (failed) failed()
        })
    }

    function loadToken() {
        call(backend.token(), function (reply) { root.token = reply.token || "" })
    }

    function connectCommand(secret) {
        return "claude mcp add --transport http logos " + (status.url || "")
            + " --header \"Authorization: Bearer " + secret + "\""
    }

    // A long account address with its middle left out: the copy button copies it whole.
    function shortAddress(address) {
        const a = String(address || "")
        return a.length > 26 ? a.slice(0, 15) + "…" + a.slice(-6) : a
    }

    // lepta as LEZ (1 LEZ = 10^9 lepta), from the integer string itself: no
    // floating point, so small and very large balances both show exactly.
    // LEZ and the Logos chain's TestToken both count 10^9 lepta to the token.
    function lez(lepta, symbol) {
        if (lepta === null || lepta === undefined || !/^[0-9]+$/.test(String(lepta))) return "?"
        const digits = String(lepta).replace(/^0+(?=.)/, "").padStart(10, "0")
        const whole = digits.slice(0, -9)
        const fraction = digits.slice(-9).replace(/0+$/, "")
        return whole + (fraction ? "." + fraction : "") + " " + (symbol || "LEZ")
    }

    // Lepta as digit strings, added exactly.
    function addLepta(a, b) {
        let sum = "", carry = 0
        a = String(a); b = String(b)
        for (let i = a.length - 1, j = b.length - 1; i >= 0 || j >= 0 || carry; i--, j--) {
            const d = (i >= 0 ? +a[i] : 0) + (j >= 0 ? +b[j] : 0) + carry
            sum = (d % 10) + sum
            carry = d >= 10 ? 1 : 0
        }
        return sum.replace(/^0+(?=.)/, "")
    }

    // ---- the approval policy draft -------------------------------------------
    function tier(name) { return Math.max(0, root.approverKeys.indexOf(name)) }

    function loadDraft() {
        const p = root.approvalPolicy
        if (!p.agent) return
        root.agentLimit = p.agent.limit_lepta
        root.agentUpTo = tier(p.agent.up_to)
        root.agentAbove = tier(p.agent.above)
        root.nodeLimit = p.node.limit_lepta
        root.nodeUpTo = tier(p.node.up_to)
        root.nodeAbove = tier(p.node.above)
        const room = p.room || {}
        root.roomTopic = room.topic || ""
        loadMembers(root.roomTopic)
        root.roomApprovers = room.approvers || []
        root.roomApprovals = room.approvals || 1
        root.roomExpires = room.expires_mins || 60
    }

    function buildPolicy() {
        return {
            agent: { limit_lepta: root.agentLimit || "0", up_to: root.approverKeys[root.agentUpTo], above: root.approverKeys[root.agentAbove] },
            node: { limit_lepta: root.nodeLimit || "0", up_to: root.approverKeys[root.nodeUpTo], above: root.approverKeys[root.nodeAbove] },
            room: root.roomTopic !== "" ? {
                topic: root.roomTopic,
                approvers: root.roomApprovers,
                approvals: root.roomApprovals,
                expires_mins: root.roomExpires
            } : null
        }
    }

    function savePolicy() {
        call(backend.saveApprovalPolicy(JSON.stringify(buildPolicy())), function () {
            root.draftDirty = false
            root.say(qsTr("Saved: transfers follow the new approvals now, waiting ones too."))
        })
    }

    // The room's members; with none trusted yet, you are ticked by default.
    // They come from the agent's seat, so they load once it runs: after
    // Basecamp starts, the seat may still be starting.
    property string membersOf: ""
    function loadMembers(topic) {
        if (topic === root.membersOf) return
        root.members = []
        root.membersOf = ""
        if (!topic || !root.seat.running) return
        root.membersOf = topic
        call(backend.roomMembers(topic), function (reply) {
            root.members = reply.members || []
            const you = root.members.filter(function (m) { return m.you && !m.agent })
            if (root.roomApprovers.length === 0 && you.length > 0) root.roomApprovers = [you[0].key]
        }, function () { root.membersOf = "" })
    }
    onSeatChanged: loadMembers(root.roomTopic)

    function toggleApprover(key, on) {
        const now = root.roomApprovers.filter(function (k) { return k !== key })
        if (on) now.push(key)
        root.roomApprovers = now
        root.roomApprovals = Math.max(1, Math.min(root.roomApprovals, now.length))
        root.draftDirty = true
    }

    // What a waiting transfer does, in words.
    function describe(request) {
        return request.chain
            ? qsTr("%1 on the Logos chain from your node's %2 to %3")
                .arg(root.lez(request.amount_lepta, "TestToken")).arg(root.shortAddress(request.from)).arg(root.shortAddress(request.to))
            : request.call
            ? qsTr("%1 from %2 %3").arg(root.lez(request.amount_lepta)).arg(root.shortAddress(request.from)).arg(request.call.description)
            : qsTr("%1 from %2 to %3").arg(root.lez(request.amount_lepta)).arg(root.shortAddress(request.from)).arg(root.shortAddress(request.to))
    }

    onApprovalPolicyChanged: if (!root.draftDirty) loadDraft()

    function time(seconds) {
        return new Date(seconds * 1000).toLocaleTimeString(Qt.locale(), Locale.ShortFormat)
    }

    Connections {
        target: logos
        function onViewModuleReadyChanged(moduleName, isReady) {
            if (moduleName !== "logos_mcp_app") return
            root.ready = isReady && root.backend !== null
            if (root.ready) root.loadToken()
        }
    }
    Component.onCompleted: {
        root.ready = root.backend !== null && logos.isViewModuleReady("logos_mcp_app")
        if (root.ready) root.loadToken()
    }

    // ---- building blocks -----------------------------------------------------

    // A rounded section with an optional title and caption.
    component Panel: Rectangle {
        id: panel
        default property alias content: body.data
        property string title: ""
        property string caption: ""
        Layout.fillWidth: true
        implicitHeight: body.implicitHeight + 2 * Theme.spacing.large
        radius: Theme.spacing.radiusLarge
        color: Theme.palette.backgroundSecondary
        border.color: Theme.palette.borderSubtle
        ColumnLayout {
            id: body
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.top: parent.top
            anchors.margins: Theme.spacing.large
            spacing: Theme.spacing.medium
            LogosText {
                visible: panel.title !== ""
                Layout.fillWidth: true
                text: panel.title
                font.pixelSize: Theme.typography.subtitleText
                font.weight: Theme.typography.weightMedium
            }
            LogosText {
                visible: panel.caption !== ""
                Layout.fillWidth: true
                Layout.topMargin: -Theme.spacing.small
                wrapMode: Text.WordWrap
                color: Theme.palette.textSecondary
                font.pixelSize: Theme.typography.secondaryText
                text: panel.caption
            }
        }
    }

    // A thin line between rows of a panel.
    component Divider: Rectangle {
        Layout.fillWidth: true
        implicitHeight: 1
        color: Theme.palette.borderHairline
    }

    // A small label over a value.
    component FieldLabel: LogosText {
        color: Theme.palette.textTertiary
        font.pixelSize: Theme.typography.secondaryText
    }

    // A centred, scrolling column for one tab.
    component Page: ScrollView {
        id: page
        default property alias content: column.data
        clip: true
        contentWidth: availableWidth
        Item {
            width: page.availableWidth
            implicitHeight: column.implicitHeight + Theme.spacing.xlarge
            ColumnLayout {
                id: column
                width: root.contentWidth
                anchors.horizontalCenter: parent.horizontalCenter
                spacing: Theme.spacing.large
            }
        }
    }

    Rectangle { anchors.fill: parent; color: Theme.palette.background }

    ColumnLayout {
        anchors.fill: parent
        anchors.topMargin: Theme.spacing.xlarge
        spacing: Theme.spacing.large

        // ---- header ----------------------------------------------------------
        ColumnLayout {
            // A layout fills its parent unless told not to: keep the header in the column.
            Layout.fillWidth: false
            Layout.preferredWidth: root.contentWidth
            Layout.alignment: Qt.AlignHCenter
            spacing: Theme.spacing.small

            RowLayout {
                Layout.fillWidth: true
                spacing: Theme.spacing.medium
                LogosText {
                    text: qsTr("Agent Access")
                    font.pixelSize: Theme.typography.pageTitleText
                    font.weight: Theme.typography.weightBold
                }
                LogosBadge {
                    Layout.alignment: Qt.AlignVCenter
                    color: !root.ready ? Theme.palette.textTertiary
                        : root.status.running ? Theme.palette.success : Theme.palette.warning
                    text: !root.ready ? qsTr("Connecting")
                        : root.status.running ? qsTr("Serving") : qsTr("Not serving")
                }
                Item { Layout.fillWidth: true }
            }
            LogosText {
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                color: Theme.palette.textSecondary
                text: !root.ready ? qsTr("Connecting to logos_mcp…")
                    : root.status.running ? qsTr("MCP endpoint at %1").arg(root.status.url)
                    : (root.status.error || qsTr("Starting the MCP endpoint…"))
            }

            LogosTabBar {
                id: tabs
                Layout.fillWidth: true
                Layout.topMargin: Theme.spacing.medium
                spacing: Theme.spacing.large
                // A notice from one tab means nothing on another.
                onCurrentIndexChanged: errorNotice.shown = false
                LogosTabButton { text: qsTr("Connect") }
                LogosTabButton { text: qsTr("Permissions") }
                LogosTabButton { text: root.waitingCount > 0 ? qsTr("Wallet · %1").arg(root.waitingCount) : qsTr("Wallet") }
                LogosTabButton { text: qsTr("Approvals") }
                LogosTabButton { text: qsTr("Activity") }
            }

            LogosNotice {
                id: errorNotice
                Layout.fillWidth: true
                severity: LogosNotice.Error
                closable: true
                shown: false
            }
        }

        StackLayout {
            Layout.fillWidth: true
            Layout.fillHeight: true
            currentIndex: tabs.currentIndex

            // ---- Connect -----------------------------------------------------
            Page {
                Panel {
                    title: qsTr("MCP endpoint")
                    caption: qsTr("Point your MCP client at this address with the token. Anyone with the token can use the tools you allow.")

                    FieldLabel { text: qsTr("Address") }
                    LogosCopyableText {
                        Layout.fillWidth: true
                        text: root.status.url || "-"
                        showCopyButton: !!root.status.url
                    }

                    FieldLabel { text: qsTr("Token") }
                    RowLayout {
                        Layout.fillWidth: true
                        LogosCopyableText {
                            Layout.fillWidth: true
                            text: root.token === "" ? "-" : root.tokenShown ? root.token : "•".repeat(24)
                            copyText: root.token
                            showCopyButton: root.token !== ""
                        }
                        LogosButton {
                            text: root.tokenShown ? qsTr("Hide") : qsTr("Show")
                            enabled: root.token !== ""
                            onClicked: root.tokenShown = !root.tokenShown
                        }
                    }
                }

                Panel {
                    title: qsTr("Claude Code")
                    caption: qsTr("Run this once to add the agent to Claude Code.")

                    Rectangle {
                        Layout.fillWidth: true
                        implicitHeight: command.implicitHeight + 2 * Theme.spacing.medium
                        radius: Theme.spacing.radiusMedium
                        color: Theme.palette.background
                        border.color: Theme.palette.borderHairline
                        RowLayout {
                            anchors.fill: parent
                            anchors.margins: Theme.spacing.medium
                            LogosText {
                                id: command
                                Layout.fillWidth: true
                                wrapMode: Text.WrapAnywhere
                                font.family: "monospace"
                                font.pixelSize: Theme.typography.secondaryText
                                color: Theme.palette.textSecondary
                                text: root.connectCommand(root.tokenShown ? root.token : "•".repeat(12))
                            }
                            LogosCopyButton {
                                Layout.alignment: Qt.AlignTop
                                enabled: root.token !== ""
                                value: root.connectCommand(root.token)
                            }
                        }
                    }
                }

                Panel {
                    title: qsTr("New token")
                    caption: qsTr("Issuing a new token stops the old one at once: every client using it has to be set up again.")
                    LogosButton {
                        text: qsTr("Issue a new token")
                        enabled: root.ready
                        onClicked: root.call(root.backend.rotateToken(), function (reply) {
                            root.token = reply.token
                            root.say(qsTr("New token issued. The old one no longer works."))
                        })
                    }
                }
            }

            // ---- Permissions -------------------------------------------------
            Page {
                Panel {
                    title: qsTr("Tools the agent may use")
                    caption: qsTr("Turned-off tools disappear from the agent's list and are refused if called. Node status is always available.")

                    Repeater {
                        model: root.groups
                        delegate: ColumnLayout {
                            required property var modelData
                            required property int index
                            Layout.fillWidth: true
                            spacing: Theme.spacing.medium
                            Divider {}
                            RowLayout {
                                Layout.fillWidth: true
                                spacing: Theme.spacing.large
                                ColumnLayout {
                                    Layout.fillWidth: true
                                    spacing: 2
                                    // fillWidth on the labels too: a fixed-size label caps the
                                    // row's maximum width, which keeps the switch from the edge.
                                    LogosText { Layout.fillWidth: true; text: modelData.title; font.weight: Theme.typography.weightMedium }
                                    LogosText {
                                        Layout.fillWidth: true
                                        wrapMode: Text.WordWrap
                                        text: modelData.detail
                                        color: Theme.palette.textSecondary
                                        font.pixelSize: Theme.typography.secondaryText
                                    }
                                }
                                // Always shows what logos_mcp saved: any flip by input snaps back,
                                // and a click only asks for the change.
                                Switch {
                                    // Tool groups are on unless saved off.
                                    readonly property bool saved: root.permissions[modelData.key] !== false
                                    checked: saved
                                    onCheckedChanged: if (checked !== saved) checked = Qt.binding(function () { return saved })
                                    enabled: root.ready
                                    Accessible.name: modelData.title
                                    onClicked: root.call(root.backend.setPermission(modelData.key, !saved), function () {})
                                }
                            }
                        }
                    }
                }
            }

            // ---- Wallet ------------------------------------------------------
            Page {
                LogosNotice {
                    Layout.fillWidth: true
                    shown: !!root.wallet.error || !!root.wallet.warning
                    severity: LogosNotice.Warning
                    message: root.wallet.error || root.wallet.warning || ""
                }

                // Transfers the agent asked for, waiting for the user's answer.
                Panel {
                    visible: (root.approvals.waiting || []).length > 0
                    title: qsTr("Waiting for you")
                    caption: qsTr("The agent asked to send these. Nothing goes out until you approve.")
                    Repeater {
                        model: root.approvals.waiting || []
                        delegate: ColumnLayout {
                            required property var modelData
                            Layout.fillWidth: true
                            spacing: Theme.spacing.medium
                            Divider {}
                            RowLayout {
                                Layout.fillWidth: true
                                spacing: Theme.spacing.medium
                                ColumnLayout {
                                    Layout.fillWidth: true
                                    spacing: 2
                                    // A program call, such as an escrow open, says what it does.
                                    // A chain transfer spends the chain node's own wallet.
                                    LogosText { Layout.fillWidth: true; wrapMode: Text.WordWrap; text: root.describe(modelData) }
                                    FieldLabel { text: qsTr("Asked at %1").arg(root.time(modelData.requested_at)) }
                                }
                                LogosButton {
                                    text: qsTr("Deny")
                                    enabled: root.ready
                                    onClicked: root.call(root.backend.deny(modelData.id), function () {
                                        root.say(qsTr("Denied: the transfer will not be sent."))
                                    })
                                }
                                LogosButton {
                                    text: qsTr("Approve")
                                    enabled: root.ready
                                    onClicked: root.call(root.backend.approve(modelData.id), function () {
                                        root.say(qsTr("Approved: sending it now."))
                                    })
                                }
                            }
                        }
                    }
                }

                // Transfers waiting for the Muster room: approved there, never here.
                Panel {
                    visible: (root.approvals.room || []).length > 0
                    title: qsTr("Waiting for the room")
                    caption: qsTr("Trusted people approve these in Muster. Cancelling closes the card there.")
                    Repeater {
                        model: root.approvals.room || []
                        delegate: ColumnLayout {
                            required property var modelData
                            Layout.fillWidth: true
                            spacing: Theme.spacing.medium
                            Divider {}
                            RowLayout {
                                Layout.fillWidth: true
                                spacing: Theme.spacing.medium
                                ColumnLayout {
                                    Layout.fillWidth: true
                                    spacing: 2
                                    LogosText { Layout.fillWidth: true; wrapMode: Text.WordWrap; text: root.describe(modelData) }
                                    FieldLabel {
                                        text: qsTr("%1 of %2 approvals · expires at %3")
                                            .arg(modelData.room.approvals).arg(modelData.room.required)
                                            .arg(root.time(modelData.room.expires_at))
                                    }
                                }
                                LogosButton {
                                    text: qsTr("Cancel")
                                    enabled: root.ready
                                    onClicked: root.call(root.backend.cancelRoomRequest(modelData.id), function () {
                                        root.say(qsTr("Cancelled: it will not be sent, and its card closes in the room."))
                                    })
                                }
                            }
                        }
                    }
                }

                Panel {
                    visible: !root.wallet.error
                    title: qsTr("Accounts")
                    caption: qsTr("The agent sends only from its agent accounts. Fund them with what it may spend.")

                    RowLayout {
                        Layout.fillWidth: true
                        FieldLabel {
                            Layout.fillWidth: true
                            elide: Text.ElideRight
                            text: root.wallet.status
                                ? qsTr("%1 · block %2").arg(root.wallet.status.sequencer).arg(root.wallet.status.block_height || "?")
                                : ""
                        }
                        LogosButton {
                            text: qsTr("New agent account")
                            enabled: root.ready
                            onClicked: root.call(root.backend.newAgentAccount(), function (reply) {
                                root.say(reply.note || qsTr("New agent account %1").arg(reply.address))
                            })
                        }
                    }

                    Repeater {
                        model: (root.wallet.accounts || []).filter(function (a) { return root.showJobAccounts || !a.job })
                        delegate: ColumnLayout {
                            required property var modelData
                            Layout.fillWidth: true
                            spacing: Theme.spacing.medium
                            Divider {}
                            RowLayout {
                                Layout.fillWidth: true
                                spacing: Theme.spacing.medium
                                ColumnLayout {
                                    Layout.fillWidth: true
                                    spacing: 4
                                    RowLayout {
                                        // Fills, so the balance and switch sit at the row's end.
                                        Layout.fillWidth: true
                                        spacing: Theme.spacing.small
                                        LogosText {
                                            font.family: "monospace"
                                            text: root.shortAddress(modelData.address || modelData.account_id)
                                        }
                                        LogosCopyButton { value: modelData.address || modelData.account_id }
                                        LogosBadge {
                                            visible: modelData.designated === true
                                            color: Theme.palette.success
                                            text: qsTr("Agent")
                                        }
                                        LogosBadge {
                                            visible: !modelData["public"]
                                            color: Theme.palette.textTertiary
                                            text: qsTr("Private")
                                        }
                                        LogosBadge {
                                            visible: !!modelData.job
                                            color: Theme.palette.info
                                            text: modelData.job ? qsTr("Job: %1").arg(modelData.job.service) : ""
                                        }
                                        Item { Layout.fillWidth: true }
                                    }
                                    LogosText {
                                        Layout.fillWidth: true
                                        visible: !!modelData.note
                                        wrapMode: Text.WordWrap
                                        color: Theme.palette.textTertiary
                                        font.pixelSize: Theme.typography.secondaryText
                                        text: modelData.note || ""
                                    }
                                }
                                // A fixed, right-aligned column, so every row's switch lines up.
                                LogosText {
                                    Layout.alignment: Qt.AlignVCenter
                                    Layout.preferredWidth: 150
                                    horizontalAlignment: Text.AlignRight
                                    font.weight: Theme.typography.weightMedium
                                    text: root.lez(modelData.balance_lepta)
                                }
                                // Transfers send from public accounts only.
                                // Same rule as the permission switches: it shows what was saved,
                                // and a click asks. Handing over needs a confirmation; taking back does not.
                                Switch {
                                    readonly property bool saved: modelData.designated === true
                                    opacity: modelData["public"] ? 1 : 0
                                    enabled: root.ready && modelData["public"]
                                    checked: saved
                                    onCheckedChanged: if (checked !== saved) checked = Qt.binding(function () { return saved })
                                    Accessible.name: qsTr("The agent may send from this account")
                                    ToolTip.visible: hovered
                                    ToolTip.text: saved ? qsTr("The agent may send from this account") : qsTr("Let the agent send from this account")
                                    onClicked: {
                                        if (!saved) {
                                            root.handOver = modelData
                                        } else {
                                            root.call(root.backend.setAgentAccount(modelData.account_id, false), function () {
                                                root.say(qsTr("The agent can no longer send from this account."))
                                            })
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Job accounts fold into one line.
                    ColumnLayout {
                        Layout.fillWidth: true
                        visible: root.jobAccounts.length > 0
                        spacing: Theme.spacing.medium
                        Divider {}
                        RowLayout {
                            Layout.fillWidth: true
                            LogosText {
                                Layout.fillWidth: true
                                wrapMode: Text.WordWrap
                                color: Theme.palette.textSecondary
                                font.pixelSize: Theme.typography.secondaryText
                                text: qsTr("Job accounts: %1, holding %2. Each service job is paid to its own account, which the agent can spend from.")
                                    .arg(root.jobAccounts.length)
                                    .arg(root.lez(root.jobAccounts.reduce(function (total, a) {
                                        return /^[0-9]+$/.test(a.balance_lepta || "") ? root.addLepta(total, a.balance_lepta) : total
                                    }, "0")))
                            }
                            LogosButton {
                                text: root.showJobAccounts ? qsTr("Hide") : qsTr("Show")
                                onClicked: root.showJobAccounts = !root.showJobAccounts
                            }
                        }
                    }

                    // Agent accounts this wallet doesn't hold: still granted, and back
                    // in force when their wallet opens, so they can be taken back here.
                    Repeater {
                        model: root.wallet.missing_agent_accounts || []
                        delegate: ColumnLayout {
                            required property var modelData
                            Layout.fillWidth: true
                            spacing: Theme.spacing.medium
                            Divider {}
                            RowLayout {
                                Layout.fillWidth: true
                                LogosText {
                                    Layout.fillWidth: true
                                    elide: Text.ElideMiddle
                                    color: Theme.palette.warning
                                    text: qsTr("Agent account not in this wallet: %1").arg(modelData)
                                }
                                LogosButton {
                                    text: qsTr("Take back")
                                    enabled: root.ready
                                    onClicked: root.call(root.backend.setAgentAccount(modelData, false), function () {
                                        root.say(qsTr("The agent can no longer send from that account."))
                                    })
                                }
                            }
                        }
                    }
                }
            }

            // ---- Approvals ---------------------------------------------------
            Page {
                LogosText {
                    Layout.fillWidth: true
                    wrapMode: Text.WordWrap
                    color: Theme.palette.textSecondary
                    text: (root.roomOn
                        ? qsTr("For each wallet, a limit splits transfers in two. Each side is sent by the agent, waits for your OK in the Wallet tab, or waits for trusted people in a Muster room.")
                        : qsTr("For each wallet, a limit splits transfers in two. Each side is sent by the agent, or waits for your OK in the Wallet tab."))
                        + " " + qsTr("The side above the limit can't be looser than the one under it. A limit of 0 puts every transfer above it.")
                }

                Repeater {
                    model: [
                        { title: qsTr("Agent wallet"), caption: qsTr("LEZ transfers, escrow payments included."), node: false },
                        { title: qsTr("Node wallet"), caption: qsTr("Logos chain transfers, from your chain node's own wallet."), node: true }
                    ]
                    delegate: Panel {
                        id: walletPanel
                        required property var modelData
                        title: modelData.title
                        caption: modelData.caption

                        GridLayout {
                            Layout.fillWidth: true
                            columns: 3
                            // Equal columns, the same in both wallets' panels.
                            uniformCellWidths: true
                            columnSpacing: Theme.spacing.large
                            rowSpacing: Theme.spacing.small

                            FieldLabel { Layout.fillWidth: true; text: qsTr("Limit (lepta)") }
                            FieldLabel { Layout.fillWidth: true; text: qsTr("Up to the limit") }
                            FieldLabel { Layout.fillWidth: true; text: qsTr("Above it") }

                            LogosTextField {
                                id: limitField
                                Layout.fillWidth: true
                                validator: RegularExpressionValidator { regularExpression: /[0-9]{1,30}/ }
                                text: walletPanel.modelData.node ? root.nodeLimit : root.agentLimit
                                Connections {
                                    target: limitField.textInput
                                    // Editing breaks the binding: bind again, so Discard shows the saved limit.
                                    function onTextEdited() {
                                        const edited = limitField.text
                                        if (walletPanel.modelData.node) root.nodeLimit = edited
                                        else root.agentLimit = edited
                                        root.draftDirty = true
                                        limitField.text = Qt.binding(function () { return walletPanel.modelData.node ? root.nodeLimit : root.agentLimit })
                                    }
                                }
                            }
                            LogosComboBox {
                                Layout.fillWidth: true
                                // As tall as the limit field beside it.
                                Layout.preferredHeight: limitField.implicitHeight
                                // Room only while room approval is on.
                                model: root.roomOn ? root.approverNames : root.approverNames.slice(0, 2)
                                function bind() {
                                    currentIndex = Qt.binding(function () { return walletPanel.modelData.node ? root.nodeUpTo : root.agentUpTo })
                                }
                                Component.onCompleted: bind()
                                // A new list resets the choice: show the draft's again.
                                onModelChanged: Qt.callLater(bind)
                                onActivated: function (index) {
                                    if (walletPanel.modelData.node) root.nodeUpTo = index
                                    else root.agentUpTo = index
                                    root.draftDirty = true
                                    bind()
                                }
                            }
                            LogosComboBox {
                                Layout.fillWidth: true
                                // As tall as the limit field beside it.
                                Layout.preferredHeight: limitField.implicitHeight
                                // Room only while room approval is on.
                                model: root.roomOn ? root.approverNames : root.approverNames.slice(0, 2)
                                function bind() {
                                    currentIndex = Qt.binding(function () { return walletPanel.modelData.node ? root.nodeAbove : root.agentAbove })
                                }
                                Component.onCompleted: bind()
                                // A new list resets the choice: show the draft's again.
                                onModelChanged: Qt.callLater(bind)
                                onActivated: function (index) {
                                    if (walletPanel.modelData.node) root.nodeAbove = index
                                    else root.agentAbove = index
                                    root.draftDirty = true
                                    bind()
                                }
                            }

                            FieldLabel {
                                Layout.columnSpan: 3
                                text: qsTr("= %1").arg(root.lez(walletPanel.modelData.node ? root.nodeLimit : root.agentLimit,
                                                                 walletPanel.modelData.node ? "TestToken" : "LEZ"))
                            }
                        }

                        LogosNotice {
                            Layout.fillWidth: true
                            severity: LogosNotice.Warning
                            shown: walletPanel.modelData.node && (root.nodeUpTo === 0 || root.nodeAbove === 0)
                            message: qsTr("With Send, the agent spends your own node wallet without asking anyone.")
                        }
                    }
                }

                // Off: one line to turn it on, and nothing asks Muster anything.
                Panel {
                    visible: !root.roomOn
                    title: qsTr("Room approval")
                    RowLayout {
                        Layout.fillWidth: true
                        spacing: Theme.spacing.large
                        LogosText {
                            Layout.fillWidth: true
                            wrapMode: Text.WordWrap
                            color: root.seat.setup_error && !root.seat.setting_up ? Theme.palette.warning : Theme.palette.textSecondary
                            text: root.seat.setup_error && !root.seat.setting_up
                                ? qsTr("Turning room approval on failed: %1").arg(root.seat.setup_error)
                                : root.seat.logosctl
                                    ? qsTr("Have trusted people approve big transfers in a Muster room. Turning it on installs Muster into a logosctl session this module runs.")
                                    : qsTr("Have trusted people approve big transfers in a Muster room: it needs logosctl installed.")
                        }
                        LogosButton {
                            // The module sets the seat up in the background; its status follows.
                            readonly property bool busy: root.settingUpSeat || !!root.seat.setting_up
                            text: busy ? qsTr("Turning on…") : qsTr("Turn on room approval")
                            enabled: root.ready && !busy && !!root.seat.logosctl
                            onClicked: {
                                root.settingUpSeat = true
                                root.call(root.backend.seatSetUp(), function () {
                                    root.settingUpSeat = false
                                }, function () { root.settingUpSeat = false })
                            }
                        }
                    }
                }

                // On: the seat, its room and whose approval counts.
                Panel {
                    visible: root.roomOn
                    title: qsTr("Room approval with Muster")
                    caption: qsTr("Trusted people approve big transfers in a Muster room. The agent joins the room as its own member, never as you, from a logosctl session this module runs in its data folder (github.com/corpetty/muster).")

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: Theme.spacing.small
                        LogosBadge {
                            color: root.seat.running ? Theme.palette.success : Theme.palette.warning
                            text: root.seat.running ? qsTr("Seat running") : qsTr("Seat starting")
                        }
                        LogosText {
                            Layout.fillWidth: true
                            elide: Text.ElideMiddle
                            font.family: "monospace"
                            color: Theme.palette.textSecondary
                            font.pixelSize: Theme.typography.secondaryText
                            text: root.seat.running ? (root.seat.key || "") : (root.seat.error || "")
                        }
                    }

                    ColumnLayout {
                        Layout.fillWidth: true
                        visible: !!root.seat.invite_id
                        spacing: Theme.spacing.small
                        FieldLabel { text: qsTr("Invite id, for Muster's Invite") }
                        LogosCopyableText {
                            Layout.fillWidth: true
                            text: root.shortAddress(root.seat.invite_id)
                            copyText: root.seat.invite_id || ""
                        }
                        FieldLabel {
                            visible: !(root.seat.rooms || []).length && !(root.seat.invites || []).length
                            text: qsTr("Invite the agent into your room from Muster with this id, then join here.")
                        }
                    }

                    Repeater {
                        model: root.seat.invites || []
                        delegate: RowLayout {
                            required property var modelData
                            Layout.fillWidth: true
                            LogosText {
                                Layout.fillWidth: true
                                wrapMode: Text.WordWrap
                                text: qsTr("Invited to %1 by %2").arg(modelData.topic).arg(modelData.from_alias || root.shortAddress(modelData.from))
                            }
                            LogosButton {
                                text: qsTr("Join")
                                enabled: root.ready
                                onClicked: root.call(root.backend.seatJoin(modelData.topic), function () {
                                    root.say(qsTr("The agent joined the room. Its members show up within a minute."))
                                })
                            }
                        }
                    }

                    Divider {}

                    FieldLabel { text: qsTr("Room") }
                    LogosComboBox {
                        id: roomPicker
                        Layout.fillWidth: true
                        // First "No room": a room tier then can't be saved, and requests waiting for a room are cancelled.
                        model: [qsTr("No room")].concat((root.seat.rooms || []).map(function (r) { return r.title ? r.title + "  " + r.topic : r.topic }))
                        currentIndex: 1 + (root.seat.rooms || []).findIndex(function (r) { return r.topic === root.roomTopic })
                        onActivated: function (index) {
                            root.roomTopic = index === 0 ? "" : root.seat.rooms[index - 1].topic
                            root.roomApprovers = []
                            root.roomApprovals = 1
                            root.draftDirty = true
                            root.loadMembers(root.roomTopic)
                            currentIndex = Qt.binding(function () {
                                return 1 + (root.seat.rooms || []).findIndex(function (r) { return r.topic === root.roomTopic })
                            })
                        }
                    }

                    FieldLabel { visible: root.members.length > 0; text: qsTr("Whose approval counts") }
                    Repeater {
                        model: root.members
                        delegate: LogosCheckbox {
                            required property var modelData
                            Layout.fillWidth: true
                            enabled: !modelData.agent
                            checked: !modelData.agent && root.roomApprovers.indexOf(modelData.key) >= 0
                            text: (modelData.agent ? qsTr("The agent (never trusted)")
                                : modelData.you ? qsTr("You")
                                : modelData.alias || modelData.key)
                            onToggled: {
                                root.toggleApprover(modelData.key, checked)
                                checked = Qt.binding(function () { return !modelData.agent && root.roomApprovers.indexOf(modelData.key) >= 0 })
                            }
                        }
                    }

                    GridLayout {
                        Layout.fillWidth: true
                        columns: 2
                        uniformCellWidths: true
                        columnSpacing: Theme.spacing.large
                        rowSpacing: Theme.spacing.small
                        FieldLabel { Layout.fillWidth: true; text: qsTr("Approvals needed") }
                        FieldLabel { Layout.fillWidth: true; text: qsTr("Waits up to (minutes)") }
                        LogosTextField {
                            id: approvalsField
                            Layout.fillWidth: true
                            validator: IntValidator { bottom: 1; top: Math.max(1, root.roomApprovers.length) }
                            text: String(root.roomApprovals)
                            Connections {
                                target: approvalsField.textInput
                                function onTextEdited() {
                                    const n = parseInt(approvalsField.text)
                                    if (n >= 1) {
                                        root.roomApprovals = n
                                        root.draftDirty = true
                                    }
                                    approvalsField.text = Qt.binding(function () { return String(root.roomApprovals) })
                                }
                            }
                        }
                        LogosTextField {
                            id: expiresField
                            Layout.fillWidth: true
                            validator: IntValidator { bottom: 1; top: 10080 }
                            text: String(root.roomExpires)
                            Connections {
                                target: expiresField.textInput
                                function onTextEdited() {
                                    const n = parseInt(expiresField.text)
                                    if (n >= 1) {
                                        root.roomExpires = n
                                        root.draftDirty = true
                                    }
                                    expiresField.text = Qt.binding(function () { return String(root.roomExpires) })
                                }
                            }
                        }
                    }

                    Divider {}

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: Theme.spacing.large
                        LogosText {
                            Layout.fillWidth: true
                            wrapMode: Text.WordWrap
                            color: Theme.palette.textSecondary
                            font.pixelSize: Theme.typography.secondaryText
                            text: qsTr("Turning it off makes room tiers ask you, cancels transfers waiting for the room, and stops the seat. Its key and rooms are kept for next time.")
                        }
                        LogosButton {
                            text: qsTr("Turn off room approval")
                            enabled: root.ready
                            onClicked: root.call(root.backend.seatTurnOff(), function () {
                                root.draftDirty = false
                                root.loadDraft()
                                root.say(qsTr("Room approval is off: room tiers ask you now."))
                            })
                        }
                    }
                }

                RowLayout {
                    Layout.fillWidth: true
                    spacing: Theme.spacing.medium
                    LogosText {
                        Layout.fillWidth: true
                        color: Theme.palette.textTertiary
                        font.pixelSize: Theme.typography.secondaryText
                        text: root.draftDirty ? qsTr("Unsaved changes") : ""
                    }
                    LogosButton {
                        text: qsTr("Discard changes")
                        enabled: root.draftDirty
                        onClicked: { root.draftDirty = false; errorNotice.shown = false; root.loadDraft() }
                    }
                    LogosButton {
                        text: qsTr("Save")
                        enabled: root.ready && root.draftDirty
                        onClicked: root.savePolicy()
                    }
                }
            }

            // ---- Activity ----------------------------------------------------
            Item {
                ColumnLayout {
                    width: root.contentWidth
                    anchors.horizontalCenter: parent.horizontalCenter
                    anchors.top: parent.top
                    anchors.bottom: parent.bottom
                    anchors.bottomMargin: Theme.spacing.xlarge
                    spacing: Theme.spacing.small

                    LogosText {
                        visible: root.activity.length === 0
                        color: Theme.palette.textSecondary
                        text: qsTr("No tool calls yet.")
                    }
                    FieldLabel {
                        visible: root.activity.length > 0
                        text: qsTr("Newest first. Click a call to see what it was given and what it returned.")
                    }

                    ListView {
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        clip: true
                        spacing: Theme.spacing.tiny
                        model: root.activity.slice().reverse()
                        ScrollBar.vertical: ScrollBar {}
                        delegate: Rectangle {
                            id: entry
                            required property var modelData
                            property bool expanded: false
                            width: ListView.view.width
                            implicitHeight: callBody.implicitHeight + 2 * Theme.spacing.medium
                            radius: Theme.spacing.radiusMedium
                            color: tap.hovered || entry.expanded ? Theme.palette.backgroundSecondary : "transparent"
                            HoverHandler { id: tap }
                            TapHandler { onTapped: entry.expanded = !entry.expanded }
                            ColumnLayout {
                                id: callBody
                                anchors.left: parent.left
                                anchors.right: parent.right
                                anchors.top: parent.top
                                anchors.margins: Theme.spacing.medium
                                spacing: Theme.spacing.tiny
                                RowLayout {
                                    Layout.fillWidth: true
                                    spacing: Theme.spacing.medium
                                    LogosText {
                                        Layout.preferredWidth: 56
                                        color: Theme.palette.textTertiary
                                        font.pixelSize: Theme.typography.secondaryText
                                        text: root.time(entry.modelData.time)
                                    }
                                    LogosText {
                                        font.family: "monospace"
                                        font.weight: Theme.typography.weightMedium
                                        text: entry.modelData.tool
                                    }
                                    LogosBadge {
                                        color: entry.modelData.ok ? Theme.palette.success : Theme.palette.error
                                        text: entry.modelData.ok ? qsTr("ok") : qsTr("failed")
                                    }
                                    LogosText {
                                        Layout.fillWidth: true
                                        visible: !entry.expanded
                                        elide: Text.ElideRight
                                        color: Theme.palette.textSecondary
                                        font.pixelSize: Theme.typography.secondaryText
                                        text: entry.modelData.result
                                    }
                                }
                                ColumnLayout {
                                    Layout.fillWidth: true
                                    Layout.leftMargin: 56 + Theme.spacing.medium
                                    visible: entry.expanded
                                    spacing: Theme.spacing.tiny
                                    FieldLabel { text: qsTr("Given") }
                                    LogosSelectableText {
                                        Layout.fillWidth: true
                                        wrapMode: Text.WrapAnywhere
                                        font.family: "monospace"
                                        font.pixelSize: Theme.typography.secondaryText
                                        text: entry.modelData.args
                                    }
                                    FieldLabel { text: qsTr("Returned") }
                                    LogosSelectableText {
                                        Layout.fillWidth: true
                                        wrapMode: Text.WrapAnywhere
                                        font.family: "monospace"
                                        font.pixelSize: Theme.typography.secondaryText
                                        text: entry.modelData.result
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Confirmations close themselves, at the bottom of the view.
    LogosToast {
        id: toast
        anchors.horizontalCenter: parent.horizontalCenter
        anchors.bottom: parent.bottom
        anchors.bottomMargin: Theme.spacing.large
        width: Math.min(parent.width - 2 * Theme.spacing.large, 560)
        severity: LogosNotice.Success
    }

    // Confirmation before handing an account to the agent. Built in-app from
    // Basecamp's own controls (a popup Dialog drew its text invisibly there), and
    // it blocks the view behind it until answered.
    Rectangle {
        anchors.fill: parent
        visible: root.handOver !== null
        color: "#b3000000"
        MouseArea { anchors.fill: parent }

        Rectangle {
            anchors.centerIn: parent
            width: Math.min(parent.width - 2 * Theme.spacing.xlarge, 520)
            height: confirmCard.implicitHeight + 2 * Theme.spacing.xlarge
            radius: Theme.spacing.radiusLarge
            color: Theme.palette.backgroundSecondary
            border.color: Theme.palette.border

            ColumnLayout {
                id: confirmCard
                anchors.fill: parent
                anchors.margins: Theme.spacing.xlarge
                spacing: Theme.spacing.large

                LogosText {
                    Layout.fillWidth: true
                    font.pixelSize: Theme.typography.subtitleText
                    font.weight: Theme.typography.weightBold
                    text: qsTr("Hand this account to the agent?")
                }
                LogosText {
                    Layout.fillWidth: true
                    wrapMode: Text.WordWrap
                    color: Theme.palette.textSecondary
                    text: root.handOver
                        ? qsTr("The agent will be able to spend everything in %1, now %2, without asking you.")
                            .arg(root.handOver.address).arg(root.lez(root.handOver.balance_lepta))
                        : ""
                }
                RowLayout {
                    Layout.alignment: Qt.AlignRight
                    spacing: Theme.spacing.medium
                    LogosButton {
                        text: qsTr("Cancel")
                        onClicked: root.handOver = null
                    }
                    LogosButton {
                        text: qsTr("Hand over")
                        onClicked: {
                            const account = root.handOver
                            root.handOver = null
                            root.call(root.backend.setAgentAccount(account.account_id, true), function () {
                                root.say(qsTr("The agent can now send from this account."))
                            })
                        }
                    }
                }
            }
        }
    }
}
