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

    property string token: ""
    property bool tokenShown: false
    property string notice: ""

    readonly property var groups: [
        { key: "messaging", title: qsTr("Messaging"), detail: qsTr("Read and send messages, RLN status") },
        { key: "storage", title: qsTr("Storage"), detail: qsTr("Upload and download files in the files folder") },
        { key: "wallet", title: qsTr("Wallet"), detail: qsTr("See accounts and balances, create agent accounts") },
        { key: "transfers", title: qsTr("Transfers"), detail: qsTr("Send LEZ from the agent accounts") },
        { key: "discovery", title: qsTr("Discovery"), detail: qsTr("Advertise and find services") }
    ]

    function parse(text, fallback) {
        try { return JSON.parse(text) } catch (e) { return fallback }
    }

    function call(pending, done) {
        logos.watch(pending, function (raw) {
            var reply = parse(raw, {})
            if (reply.error) root.notice = reply.error
            else done(reply)
        }, function (error) { root.notice = String(error) })
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
                        Switch {
                            checked: root.permissions[modelData.key] !== false
                            enabled: root.ready
                            onToggled: {
                                root.call(root.backend.setPermission(modelData.key, checked), function () {})
                                // A toggle replaces the binding: put it back, so the switch
                                // always shows what logos_mcp saved, even if saving failed.
                                checked = Qt.binding(function () { return root.permissions[modelData.key] !== false })
                            }
                        }
                    }
                }

                Item { Layout.fillHeight: true }
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
}
