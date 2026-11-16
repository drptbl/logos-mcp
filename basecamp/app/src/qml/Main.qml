import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Logos.Theme
import Logos.Controls

// The Agent app: connect an MCP client, choose the tool groups the agent may
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
    readonly property var approverKeys: ["send", "ask", "room"]
    readonly property var approverNames: [qsTr("Send"), qsTr("Ask me"), qsTr("Room")]
    // The account waiting for the user to confirm handing it over.
    property var handOver: null

    property string token: ""
    property bool tokenShown: false
    property string notice: ""
    // Accounts made to be paid for one service job stay folded into one line.
    property bool showJobAccounts: false
    readonly property var jobAccounts: (wallet.accounts || []).filter(function (a) { return a.job })

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

    // `failed`, if given, runs after an error is shown.
    function call(pending, done, failed) {
        logos.watch(pending, function (raw) {
            var reply = parse(raw, {})
            if (reply.error) {
                root.notice = reply.error
                if (failed) failed()
            } else done(reply)
        }, function (error) {
            root.notice = String(error)
            if (failed) failed()
        })
    }

    function loadToken() {
        call(backend.token(), function (reply) { root.token = reply.token || "" })
    }

    function copy(text) {
        clipboard.text = text
        clipboard.selectAll()
        clipboard.copy()
        root.notice = qsTr("Copied")
    }

    function connectCommand() {
        return "claude mcp add --transport http logos " + (status.url || "")
            + " --header \"Authorization: Bearer " + token + "\""
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
        if (room.topic && room.topic !== root.roomTopic) loadMembers(room.topic)
        root.roomTopic = room.topic || ""
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
            root.notice = qsTr("Saved: transfers follow the new approvals now, waiting ones too.")
        })
    }

    // The room's members; with none trusted yet, you are ticked by default.
    function loadMembers(topic) {
        root.members = []
        if (!topic) return
        call(backend.roomMembers(topic), function (reply) {
            root.members = reply.members || []
            const you = root.members.filter(function (m) { return m.you && !m.agent })
            if (root.roomApprovers.length === 0 && you.length > 0) root.roomApprovers = [you[0].key]
        })
    }

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
                .arg(root.lez(request.amount_lepta, "TestToken")).arg(request.from).arg(request.to)
            : request.call
            ? qsTr("%1 from %2 %3").arg(root.lez(request.amount_lepta)).arg(request.from).arg(request.call.description)
            : qsTr("%1 from %2 to %3").arg(root.lez(request.amount_lepta)).arg(request.from).arg(request.to)
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

    // QML's own clipboard access: the backend runs headless.
    TextEdit { id: clipboard; visible: false }

    Rectangle { anchors.fill: parent; color: Theme.palette.background }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: Theme.spacing.xlarge
        spacing: Theme.spacing.large

        LogosText {
            text: qsTr("AI Agent")
            font.pixelSize: Theme.typography.titleText
            font.weight: Theme.typography.weightBold
        }

        LogosText {
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            color: root.status.running ? Theme.palette.success : Theme.palette.warning
            text: !root.ready ? qsTr("Connecting to logos_mcp…")
                : root.status.running ? qsTr("Serving MCP at %1").arg(root.status.url)
                : qsTr("Not serving: %1").arg(root.status.error || qsTr("starting"))
        }

        TabBar {
            id: tabs
            Layout.fillWidth: true
            TabButton { text: qsTr("Connect") }
            TabButton { text: qsTr("Permissions") }
            TabButton { text: qsTr("Wallet") }
            TabButton { text: qsTr("Approvals") }
            TabButton { text: qsTr("Activity") }
        }

        StackLayout {
            Layout.fillWidth: true
            Layout.fillHeight: true
            currentIndex: tabs.currentIndex

            // ---- Connect -------------------------------------------------
            ColumnLayout {
                spacing: Theme.spacing.medium

                LogosText {
                    Layout.fillWidth: true
                    wrapMode: Text.WordWrap
                    color: Theme.palette.textSecondary
                    text: qsTr("Point your MCP client at this address with the token below. Anyone with the token can use the tools you allow.")
                }

                LogosText { text: qsTr("Address"); font.weight: Theme.typography.weightMedium }
                RowLayout {
                    LogosText { text: root.status.url || "-"; Layout.fillWidth: true }
                    LogosButton { text: qsTr("Copy"); enabled: !!root.status.url; onClicked: root.copy(root.status.url) }
                }

                LogosText { text: qsTr("Token"); font.weight: Theme.typography.weightMedium }
                RowLayout {
                    LogosText {
                        Layout.fillWidth: true
                        elide: Text.ElideMiddle
                        font.family: "monospace"
                        text: root.token === "" ? "-" : root.tokenShown ? root.token : "•".repeat(24)
                    }
                    LogosButton { text: root.tokenShown ? qsTr("Hide") : qsTr("Show"); onClicked: root.tokenShown = !root.tokenShown }
                    LogosButton { text: qsTr("Copy"); enabled: root.token !== ""; onClicked: root.copy(root.token) }
                }

                LogosText { text: qsTr("Claude Code"); font.weight: Theme.typography.weightMedium }
                RowLayout {
                    LogosText {
                        Layout.fillWidth: true
                        elide: Text.ElideRight
                        font.family: "monospace"
                        color: Theme.palette.textSecondary
                        text: "claude mcp add --transport http logos " + (root.status.url || "") + " --header …"
                    }
                    LogosButton { text: qsTr("Copy command"); enabled: root.token !== ""; onClicked: root.copy(root.connectCommand()) }
                }

                LogosButton {
                    text: qsTr("Issue a new token")
                    onClicked: root.call(root.backend.rotateToken(), function (reply) {
                        root.token = reply.token
                        root.notice = qsTr("New token issued. The old one no longer works.")
                    })
                }

                Item { Layout.fillHeight: true }
            }

            // ---- Permissions ---------------------------------------------
            ColumnLayout {
                spacing: Theme.spacing.medium

                LogosText {
                    Layout.fillWidth: true
                    wrapMode: Text.WordWrap
                    color: Theme.palette.textSecondary
                    text: qsTr("Turned-off tools disappear from the agent's list and are refused if called. Node status is always available.")
                }

                Repeater {
                    model: root.groups
                    delegate: RowLayout {
                        required property var modelData
                        Layout.fillWidth: true
                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: 2
                            // fillWidth on the labels too: a fixed-size label caps the
                            // row's maximum width, which keeps the switch from the edge.
                            LogosText { Layout.fillWidth: true; text: modelData.title; font.weight: Theme.typography.weightMedium }
                            LogosText { Layout.fillWidth: true; text: modelData.detail; color: Theme.palette.textSecondary; font.pixelSize: Theme.typography.secondaryText }
                        }
                        // Always shows what logos_mcp saved: any flip by input snaps back,
                        // and a click only asks for the change.
                        Switch {
                            // Tool groups are on unless saved off.
                            readonly property bool saved: root.permissions[modelData.key] !== false
                            checked: saved
                            onCheckedChanged: if (checked !== saved) checked = Qt.binding(function () { return saved })
                            enabled: root.ready
                            onClicked: root.call(root.backend.setPermission(modelData.key, !saved), function () {})
                        }
                    }
                }

                Item { Layout.fillHeight: true }
            }

            // ---- Wallet --------------------------------------------------
            ColumnLayout {
                spacing: Theme.spacing.medium

                LogosText {
                    Layout.fillWidth: true
                    wrapMode: Text.WordWrap
                    color: root.wallet.error ? Theme.palette.warning : Theme.palette.textSecondary
                    text: root.wallet.error ? root.wallet.error
                        : qsTr("The agent sends only from its agent accounts. Fund them with what it may spend.")
                }

                LogosText {
                    Layout.fillWidth: true
                    wrapMode: Text.WordWrap
                    visible: !!root.wallet.warning
                    color: Theme.palette.warning
                    text: root.wallet.warning || ""
                }

                RowLayout {
                    visible: !root.wallet.error
                    Layout.fillWidth: true
                    LogosText {
                        Layout.fillWidth: true
                        color: Theme.palette.textTertiary
                        text: root.wallet.status
                            ? qsTr("%1, block %2").arg(root.wallet.status.sequencer).arg(root.wallet.status.block_height || "?")
                            : ""
                    }
                    LogosButton {
                        text: qsTr("New agent account")
                        enabled: root.ready
                        onClicked: root.call(root.backend.newAgentAccount(), function (reply) {
                            root.notice = reply.note || qsTr("New agent account %1").arg(reply.address)
                        })
                    }
                }

                // Transfers the agent asked for, waiting for the user's answer.
                LogosText {
                    visible: (root.approvals.waiting || []).length > 0
                    font.weight: Theme.typography.weightBold
                    color: Theme.palette.warning
                    text: qsTr("Waiting for you")
                }
                Repeater {
                    model: root.approvals.waiting || []
                    delegate: RowLayout {
                        required property var modelData
                        Layout.fillWidth: true
                        LogosText {
                            Layout.fillWidth: true
                            wrapMode: Text.WordWrap
                            // A program call, such as an escrow open, says what it does.
                            // A chain transfer spends the chain node's own wallet.
                            text: qsTr("%1, asked at %2").arg(root.describe(modelData)).arg(root.time(modelData.requested_at))
                        }
                        LogosButton {
                            text: qsTr("Deny")
                            enabled: root.ready
                            onClicked: root.call(root.backend.deny(modelData.id), function () {
                                root.notice = qsTr("Denied: the transfer will not be sent.")
                            })
                        }
                        LogosButton {
                            text: qsTr("Approve")
                            enabled: root.ready
                            onClicked: root.call(root.backend.approve(modelData.id), function () {
                                root.notice = qsTr("Approved: sending it now.")
                            })
                        }
                    }
                }

                // Transfers waiting for the Muster room: approved there, never here.
                LogosText {
                    visible: (root.approvals.room || []).length > 0
                    font.weight: Theme.typography.weightBold
                    color: Theme.palette.warning
                    text: qsTr("Waiting for the room")
                }
                Repeater {
                    model: root.approvals.room || []
                    delegate: RowLayout {
                        required property var modelData
                        Layout.fillWidth: true
                        LogosText {
                            Layout.fillWidth: true
                            wrapMode: Text.WordWrap
                            text: qsTr("%1. %2 of %3 approvals, approve it in Muster. Expires at %4")
                                .arg(root.describe(modelData))
                                .arg(modelData.room.approvals).arg(modelData.room.required)
                                .arg(root.time(modelData.room.expires_at))
                        }
                        LogosButton {
                            text: qsTr("Cancel")
                            enabled: root.ready
                            onClicked: root.call(root.backend.cancelRoomRequest(modelData.id), function () {
                                root.notice = qsTr("Cancelled: it will not be sent, and its card closes in the room.")
                            })
                        }
                    }
                }

                // Agent accounts this wallet doesn't hold: still granted, and back
                // in force when their wallet opens, so they can be taken back here.
                Repeater {
                    model: root.wallet.missing_agent_accounts || []
                    delegate: RowLayout {
                        required property var modelData
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
                                root.notice = qsTr("The agent can no longer send from that account.")
                            })
                        }
                    }
                }

                RowLayout {
                    Layout.fillWidth: true
                    visible: root.jobAccounts.length > 0
                    LogosText {
                        Layout.fillWidth: true
                        wrapMode: Text.WordWrap
                        color: Theme.palette.textSecondary
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

                ListView {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    clip: true
                    spacing: Theme.spacing.small
                    model: (root.wallet.accounts || []).filter(function (a) { return root.showJobAccounts || !a.job })
                    delegate: RowLayout {
                        required property var modelData
                        width: ListView.view.width
                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: 2
                            LogosText {
                                Layout.fillWidth: true
                                elide: Text.ElideMiddle
                                font.family: "monospace"
                                text: modelData.address || modelData.account_id
                            }
                            LogosText {
                                Layout.fillWidth: true
                                color: Theme.palette.textSecondary
                                font.pixelSize: Theme.typography.secondaryText
                                wrapMode: Text.WordWrap
                                text: root.lez(modelData.balance_lepta)
                                    + (modelData["public"] ? "" : qsTr(", private"))
                                    + (modelData.designated ? qsTr(", agent account") : "")
                                    + (modelData.job ? qsTr(", job account for %1").arg(modelData.job.service) : "")
                                    + (modelData.note ? " (" + modelData.note + ")" : "")
                            }
                        }
                        LogosButton {
                            text: qsTr("Copy")
                            onClicked: root.copy(modelData.address || modelData.account_id)
                        }
                        // Transfers send from public accounts only.
                        // Same rule as the permission switches: it shows what was saved,
                        // and a click asks. Handing over needs a confirmation; taking back does not.
                        Switch {
                            readonly property bool saved: modelData.designated === true
                            visible: modelData["public"]
                            checked: saved
                            onCheckedChanged: if (checked !== saved) checked = Qt.binding(function () { return saved })
                            enabled: root.ready
                            onClicked: {
                                if (!saved) {
                                    root.handOver = modelData
                                } else {
                                    root.call(root.backend.setAgentAccount(modelData.account_id, false), function () {
                                        root.notice = qsTr("The agent can no longer send from this account.")
                                    })
                                }
                            }
                        }
                    }
                }
            }

            // ---- Approvals -----------------------------------------------
            ScrollView {
                id: approvalsView
                clip: true
                contentWidth: availableWidth

                ColumnLayout {
                    width: approvalsView.availableWidth
                    spacing: Theme.spacing.medium

                    LogosText {
                        Layout.fillWidth: true
                        wrapMode: Text.WordWrap
                        color: Theme.palette.textSecondary
                        text: qsTr("For each wallet, a limit splits transfers in two. Each side is sent by the agent, waits for your OK in the Wallet tab, or waits for trusted people in a Muster room. The side above the limit can't be looser than the one under it. A limit of 0 puts every transfer above it.")
                    }

                    Repeater {
                        model: [
                            { title: qsTr("Agent wallet (LEZ, escrow payments included)"), node: false },
                            { title: qsTr("Node wallet (Logos chain transfers)"), node: true }
                        ]
                        delegate: ColumnLayout {
                            required property var modelData
                            Layout.fillWidth: true
                            spacing: Theme.spacing.small
                            LogosText { text: modelData.title; font.weight: Theme.typography.weightMedium }
                            RowLayout {
                                Layout.fillWidth: true
                                LogosText { text: qsTr("Limit (lepta)") }
                                TextField {
                                    Layout.preferredWidth: 160
                                    validator: RegularExpressionValidator { regularExpression: /[0-9]{1,30}/ }
                                    text: modelData.node ? root.nodeLimit : root.agentLimit
                                    // Editing breaks the binding: bind again, so Discard shows the saved limit.
                                    onTextEdited: {
                                        if (modelData.node) root.nodeLimit = text
                                        else root.agentLimit = text
                                        root.draftDirty = true
                                        text = Qt.binding(function () { return modelData.node ? root.nodeLimit : root.agentLimit })
                                    }
                                }
                                LogosText { text: qsTr("Up to it") }
                                ComboBox {
                                    model: root.approverNames
                                    currentIndex: modelData.node ? root.nodeUpTo : root.agentUpTo
                                    onActivated: function (index) {
                                        if (modelData.node) root.nodeUpTo = index
                                        else root.agentUpTo = index
                                        root.draftDirty = true
                                        currentIndex = Qt.binding(function () { return modelData.node ? root.nodeUpTo : root.agentUpTo })
                                    }
                                }
                                LogosText { text: qsTr("Above it") }
                                ComboBox {
                                    model: root.approverNames
                                    currentIndex: modelData.node ? root.nodeAbove : root.agentAbove
                                    onActivated: function (index) {
                                        if (modelData.node) root.nodeAbove = index
                                        else root.agentAbove = index
                                        root.draftDirty = true
                                        currentIndex = Qt.binding(function () { return modelData.node ? root.nodeAbove : root.agentAbove })
                                    }
                                }
                            }
                            LogosText {
                                Layout.fillWidth: true
                                wrapMode: Text.WordWrap
                                color: Theme.palette.warning
                                visible: modelData.node && (root.nodeUpTo === 0 || root.nodeAbove === 0)
                                text: qsTr("With Send, the agent spends your own node wallet without asking anyone.")
                            }
                        }
                    }

                    LogosText { text: qsTr("The agent's seat in Muster"); font.weight: Theme.typography.weightMedium }
                    LogosText {
                        Layout.fillWidth: true
                        wrapMode: Text.WordWrap
                        color: Theme.palette.textSecondary
                        text: qsTr("A room approves with the agent as its own member, never as you. Its seat is a logosctl session the module runs in its data folder.")
                    }
                    LogosText {
                        Layout.fillWidth: true
                        wrapMode: Text.WordWrap
                        visible: !root.seat.logosctl
                        color: Theme.palette.warning
                        text: qsTr("logosctl isn't installed: install it first, then come back here.")
                    }
                    RowLayout {
                        visible: !!root.seat.logosctl && !root.seat.set_up
                        Layout.fillWidth: true
                        LogosText {
                            Layout.fillWidth: true
                            wrapMode: Text.WordWrap
                            text: qsTr("Sets up the seat: installs Muster and the six modules it needs from Muster's catalogue (github.com/corpetty/muster) into its own session, and runs it while this module runs.")
                        }
                        LogosButton {
                            // The module sets the seat up in the background; its status follows.
                            readonly property bool busy: root.settingUpSeat || !!root.seat.setting_up
                            text: busy ? qsTr("Setting up…") : qsTr("Set up agent seat")
                            enabled: root.ready && !busy
                            onClicked: {
                                root.settingUpSeat = true
                                root.call(root.backend.seatSetUp(), function () {
                                    root.settingUpSeat = false
                                }, function () { root.settingUpSeat = false })
                            }
                        }
                    }
                    LogosText {
                        Layout.fillWidth: true
                        wrapMode: Text.WordWrap
                        visible: !root.seat.set_up && !root.seat.setting_up && !!root.seat.setup_error
                        color: Theme.palette.warning
                        text: qsTr("Setting up the seat failed: %1").arg(root.seat.setup_error || "")
                    }
                    LogosText {
                        Layout.fillWidth: true
                        wrapMode: Text.WordWrap
                        visible: !!root.seat.set_up
                        color: root.seat.running ? Theme.palette.success : Theme.palette.warning
                        text: root.seat.running ? qsTr("The seat is running as %1.").arg(root.seat.key || "")
                            : qsTr("The seat isn't running: %1").arg(root.seat.error || qsTr("starting"))
                    }
                    RowLayout {
                        visible: !!root.seat.invite_id
                        Layout.fillWidth: true
                        LogosText {
                            Layout.fillWidth: true
                            elide: Text.ElideMiddle
                            font.family: "monospace"
                            text: qsTr("Invite id: %1").arg(root.seat.invite_id || "")
                        }
                        LogosButton { text: qsTr("Copy"); onClicked: root.copy(root.seat.invite_id) }
                    }
                    Repeater {
                        model: root.seat.invites || []
                        delegate: RowLayout {
                            required property var modelData
                            Layout.fillWidth: true
                            LogosText {
                                Layout.fillWidth: true
                                wrapMode: Text.WordWrap
                                text: qsTr("Invited to %1 by %2").arg(modelData.topic).arg(modelData.from_alias || modelData.from)
                            }
                            LogosButton {
                                text: qsTr("Join")
                                enabled: root.ready
                                onClicked: root.call(root.backend.seatJoin(modelData.topic), function () {
                                    root.notice = qsTr("The agent joined the room. Its members show up within a minute.")
                                })
                            }
                        }
                    }

                    LogosText { text: qsTr("Room"); font.weight: Theme.typography.weightMedium }
                    ComboBox {
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
                    Repeater {
                        model: root.members
                        // The label is LogosText: CheckBox's own text takes the style's
                        // dark colour, unreadable on Basecamp's dark background.
                        delegate: RowLayout {
                            required property var modelData
                            CheckBox {
                                enabled: !modelData.agent
                                checked: !modelData.agent && root.roomApprovers.indexOf(modelData.key) >= 0
                                onToggled: {
                                    root.toggleApprover(modelData.key, checked)
                                    checked = Qt.binding(function () { return !modelData.agent && root.roomApprovers.indexOf(modelData.key) >= 0 })
                                }
                            }
                            LogosText {
                                Layout.fillWidth: true
                                elide: Text.ElideMiddle
                                color: modelData.agent ? Theme.palette.textTertiary : Theme.palette.text
                                text: (modelData.agent ? qsTr("The agent (never trusted)")
                                    : modelData.you ? qsTr("You")
                                    : modelData.alias || modelData.key)
                            }
                        }
                    }
                    RowLayout {
                        LogosText { text: qsTr("Approvals needed") }
                        SpinBox {
                            from: 1
                            to: Math.max(1, root.roomApprovers.length)
                            value: root.roomApprovals
                            onValueModified: {
                                root.roomApprovals = value
                                root.draftDirty = true
                                value = Qt.binding(function () { return root.roomApprovals })
                            }
                        }
                        LogosText { text: qsTr("Waits up to (minutes)") }
                        SpinBox {
                            from: 1
                            to: 10080
                            value: root.roomExpires
                            onValueModified: {
                                root.roomExpires = value
                                root.draftDirty = true
                                value = Qt.binding(function () { return root.roomExpires })
                            }
                        }
                    }

                    RowLayout {
                        LogosButton {
                            text: qsTr("Save")
                            enabled: root.ready && root.draftDirty
                            onClicked: root.savePolicy()
                        }
                        LogosButton {
                            text: qsTr("Discard changes")
                            enabled: root.draftDirty
                            // A refusal of the discarded edit no longer applies.
                            onClicked: { root.draftDirty = false; root.notice = ""; root.loadDraft() }
                        }
                    }
                }
            }

            // ---- Activity ------------------------------------------------
            ColumnLayout {
                spacing: Theme.spacing.small

                LogosText {
                    visible: root.activity.length === 0
                    color: Theme.palette.textSecondary
                    text: qsTr("No tool calls yet.")
                }

                ListView {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    clip: true
                    spacing: Theme.spacing.small
                    model: root.activity.slice().reverse()
                    delegate: ColumnLayout {
                        required property var modelData
                        width: ListView.view.width
                        spacing: 2
                        RowLayout {
                            LogosText { text: root.time(modelData.time); color: Theme.palette.textTertiary }
                            LogosText { text: modelData.tool; font.weight: Theme.typography.weightMedium }
                            LogosText {
                                text: modelData.ok ? qsTr("ok") : qsTr("failed")
                                color: modelData.ok ? Theme.palette.success : Theme.palette.error
                            }
                        }
                        LogosText {
                            Layout.fillWidth: true
                            elide: Text.ElideRight
                            color: Theme.palette.textSecondary
                            font.pixelSize: Theme.typography.secondaryText
                            text: modelData.args + "  →  " + modelData.result
                        }
                    }
                }
            }
        }

        LogosText {
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            visible: root.notice !== ""
            color: Theme.palette.textSecondary
            text: root.notice
        }
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
            color: Theme.palette.backgroundTertiary
            border.color: Theme.palette.border

            ColumnLayout {
                id: confirmCard
                anchors.fill: parent
                anchors.margins: Theme.spacing.xlarge
                spacing: Theme.spacing.large

                LogosText {
                    Layout.fillWidth: true
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
                                root.notice = qsTr("The agent can now send from this account.")
                            })
                        }
                    }
                }
            }
        }
    }
}
