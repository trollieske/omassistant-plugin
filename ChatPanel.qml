import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui

// The visible half of Omassistant: one card over a dimmed desktop, drawn in the
// shell's own surface language (menu tokens, monospace, hairlines, no rounding
// unless the compositor rounds).
//
// Keyboard-first: the composer always holds focus, Enter sends, Esc closes,
// Ctrl+C stops a run that is still streaming, Ctrl+K starts a new
// conversation. The mouse can dismiss.
//
// The card hugs its content: it opens at its smallest, grows upward as an
// answer arrives while the composer stays exactly where it was, and scrolls
// once it reaches the height the screen can spare.
Item {
  id: root

  // A plain ListModel owned by the host: kind, label, text, streaming, failed.
  property var transcript: null
  property bool active: false
  property bool busy: false
  property bool runtimeLive: false
  property bool runtimeStarting: false
  property bool waiting: false
  property string runtimeFault: ""
  property string modelLabel: ""
  // The runtime stopped for good and the person has to fix something outside
  // the chat (usually: pi is missing, or pi has no usable model). A slow first
  // start is not a setup problem; the card says "connecting" while mise may be
  // installing pi.
  readonly property bool setupMode: root.runtimeFault.length > 0
    && !root.runtimeLive && !root.runtimeStarting && !root.busy
  readonly property bool setupMissing: root.setupMode
    && /(failed to execute pi|no such file|command not found|not installed|did not start.*installed|could not be started)/i
      .test(root.runtimeFault)
  readonly property string setupTitle: root.setupMissing
    ? "Omassistant needs pi" : "The assistant could not start"
  readonly property string setupBody: root.setupMissing
    ? "pi is the agent runtime that talks to your model. It is not bundled with Omassistant. Omarchy manages it through mise; if it is missing, install it once and come back."
    : "pi is the agent runtime. It started but did not become usable. Check the message below, then fix the model or provider and try again."
  // The session index from the host: file/title/when/turns/current.
  property var historyModel: null
  property bool historyReady: true
  property string currentSession: ""
  property bool historyMode: false
  property int historyIndex: 0
  // "" | "consent" | "looking" | "attached"
  property string lookState: ""
  property string consentWindow: ""
  // Voxtype's state: idle | recording | transcribing.
  property string dictation: "idle"
  readonly property bool recording: root.dictation === "recording"
  readonly property bool transcribing: root.dictation === "transcribing"
  // One brokered action at a time: confirm, then run.
  property string actionState: ""
  property string actionTitle: ""
  readonly property bool actionConfirm: root.actionState === "confirm"
  readonly property bool actionRunning: root.actionState === "running"
  readonly property bool actionMode: root.actionState !== ""
  // A tool call from the agent, waiting for the person's answer.
  property bool brokerConfirm: false
  property string brokerSummary: ""
  property bool brokerActing: false
  property bool grantsOn: false
  property bool trusted: false
  property bool localReachable: false
  property string localEndpoint: ""
  // The agent is acting: the sidebar stays, but the app owns the keyboard.
  property bool actingFocus: false
  // The model settings sheet, drawn lazily so a broken panel cannot take the
  // chat down with it.
  property bool settingsOpen: false
  property bool welcomeOpen: false
  property var welcomeState: ({})
  property var settingsState: ({})
  property string webProvider: "keyless"
  property bool webKeySet: false
  // Questions already asked, newest last; and where Up has walked to.
  property var sentHistory: []
  // The picture a look would send, shown before it is approved.
  property string brokerCapability: ""
  property bool previewReady: false
  property string previewUrl: ""
  property string previewCaption: ""
  // Pictures the search behind the last answer came with (club badges, mostly).
  property var mediaImages: []
  // What the run is doing, from the broker's audit: the steps are real, and the
  // click count is the proof that most of them were not clicks.
  property var runSteps: []
  property int runClicks: 0
  property int recallIndex: -1
  property string recallDraft: ""
  property bool permissionsOpen: false
  property var permissionsState: ({})
  property bool costsOpen: false
  property var costsState: ({})
  property string grantLabel: ""
  readonly property bool confirmVisible: root.actionConfirm || root.brokerConfirm
  // True for the few frames of a capture: the card and scrim stop painting so
  // the screenshot contains the window, not this overlay.
  property bool blank: false
  readonly property bool consentMode: root.lookState === "consent"
  // One question at a time. The rows all fill the composer's slot, so more than
  // one visible drew them on top of each other: "ENTER ALLOW · CTRL+P …" and
  // "ENTER ALLOW · ESC DENY" landed in the same pixels. Priority: the agent's
  // tool call (it is already waiting on us), then the card's own action, then a
  // look. Each row's own visibility asks this.
  readonly property bool lookConsentVisible: root.consentMode && !root.brokerConfirm
    && !root.actionConfirm
  readonly property bool actionConfirmVisible: root.actionConfirm && !root.brokerConfirm
  readonly property bool anyConsent: root.brokerConfirm || root.actionConfirm || root.consentMode
  readonly property bool looking: root.lookState === "looking"

  signal submitted(string text)
  signal welcomeClosed()
  signal welcomeAsked(string prompt)
  signal welcomeRequested()
  signal settingsAutoPicksChanged(bool on)
  signal dismissed()
  signal aborted()
  signal cleared()
  signal sessionChosen(string file)
  signal lookRequested(string draft)
  signal lookAllowed()
  signal lookCancelled()
  signal dictationToggled()
  signal actionAllowed()
  signal actionCancelled()
  signal brokerAllowed()
  signal brokerDenied()
  signal stopRequested()
  signal settingsRequested()
  signal settingsClosed()
  signal settingsModelChosen(string role, string provider, string id)
  signal settingsAutoChanged(bool on)
  signal settingsWebProviderChosen(string provider)
  // A click on the card is a request for the keyboard, even while an action has
  // it: the card must never be a window with no way back in.
  signal actingFocusRelease()
  signal previewRequested()
  signal settingsWebKeySaved(string key)
  signal settingsTrustChanged(bool on)
  signal settingsLocalOnlyChanged(bool on)
  signal settingsDiscover()
  signal settingsRefresh()
  signal settingsDesktopChanged(bool on)
  signal permissionsRequested()
  signal permissionsClosed()
  signal permissionsGrant(string capability, int seconds)
  signal permissionsGrantAll(int seconds)
  signal permissionsRevoke(string capability)
  signal permissionsRevokeAll()
  signal permissionsRefresh()
  signal costsRequested()
  signal costsClosed()
  signal costsRefresh()
  signal setupRetry()
  signal setupCopy()
  signal setupOpen()

  property color surface: Commons.Color.menu.background
  property color foreground: Commons.Color.menu.text
  property color border: Commons.Color.menu.border
  property var borderSpec: Border.surfaceSpec("menu", "border", border, Math.max(1, Style.space(2)))
  property color scrim: Commons.Color.menu.scrim
  property color accent: Commons.Color.accent
  property color urgent: Commons.Color.urgent
  property color questionFill: Commons.Color.menu.selectedBackground
  property color questionText: Commons.Color.menu.text
  // Three legible tiers, never a dim color multiplied by opacity again.
  readonly property color textSecondary: Commons.Util.alpha(root.foreground, 0.78)
  readonly property color textTertiary: Commons.Util.alpha(root.foreground, 0.62)
  readonly property color textPlaceholder: Commons.Util.alpha(root.foreground, 0.58)
  property string fontFamily: Style.font.menuFamily

  readonly property int cornerRadius: Style.cornerRadius
  readonly property int contentMargin: Style.spacing.panelPadding
  readonly property int headerHeight: Math.max(Style.space(34), Style.font.title + Style.spacing.controlPaddingY * 2)
  // A confirmation that names a whole command needs the room to show it: the row
  // grows to the text instead of eliding what is being approved.
  readonly property int brokerSummaryLines: {
    if (!root.brokerConfirm) return 1
    var chars = String(root.brokerSummary || "").length
    return Math.max(1, Math.min(5, Math.ceil(chars / 68)))
  }
  readonly property int composerHeight: root.brokerConfirm
    ? Style.space(10) + root.brokerSummaryLines * (Style.font.body + Style.space(5))
    : Math.max(Style.space(40), Style.font.heading + Style.spacing.inputPaddingY * 2)
  readonly property int footerHeight: Math.max(Style.space(20), Style.font.caption + Style.spacing.md)
  readonly property int sectionGap: Style.spacing.xxl
  readonly property int messageGap: Style.space(7)
  readonly property int turnGap: Style.space(22)
  // Where text starts, so the typed line and every answer share one column.
  readonly property real gutter: Style.space(22)
  // Transcript type: a fixed leading keeps a long answer compact and predictable
  // whatever family the theme resolves "monospace" to.
  readonly property int bodyFontSize: Style.font.title
  readonly property int bodyLeading: Math.round(root.bodyFontSize * 1.45)
  readonly property int questionLeading: Math.round(root.bodyFontSize * 1.35)

  readonly property real cardWidth: Math.max(Style.space(380),
    Math.min(Style.space(560), root.width * 0.38))
  // A top popover: under the bar, not centred over the app. The app below stays
  // visible through the light scrim; the card scrolls instead of growing over it.
  readonly property real topMargin: Style.space(48)
  readonly property real bodyMinHeight: Style.space(96)

  // Frame (border + padding) plus every fixed row around the transcript.
  readonly property real chromeHeight: card.contentTopInset + card.contentBottomInset
    + root.headerHeight + root.sectionGap * 3
    + root.composerHeight + Style.spacing.md + 1 + root.footerHeight
  readonly property real emptyHeight: root.chromeHeight + root.bodyMinHeight
  // A right sidebar, not a popover: full height under the bar, full width fixed.
  readonly property real cardMaxHeight: Math.max(Style.space(300),
    root.height - root.topMargin - Style.space(16))
  readonly property real cardMinHeight: root.emptyHeight

  readonly property real bodyContentHeight: transcriptColumn.implicitHeight
  readonly property real historyPaneHeight: root.historyMode && root.historyModel
    ? Math.max(Style.space(140), Math.min(root.historyModel.count, 10) * root.historyRowHeight) : 0
  readonly property real settingsMinHeight: Style.space(430)
  readonly property real permissionsMinHeight: Style.space(700)
  readonly property real wantedHeight: root.chromeHeight
    + Math.max(root.bodyMinHeight, root.bodyContentHeight, root.historyPaneHeight,
      root.settingsOpen ? root.settingsMinHeight
        : (root.permissionsOpen ? root.permissionsMinHeight : 0))

  property real cardHeight: root.cardMaxHeight
  Behavior on cardHeight {
    // Chasing a streaming height reads as a card that breathes; snap while the
    // answer grows and animate the discrete moves (open, a new turn).
    enabled: !root.busy
    NumberAnimation {
      duration: Style.duration(200)
      easing.type: Easing.Bezier
      easing.bezierCurve: Style.easeOut
    }
  }

  // Opening is the only transition: closing is immediate, the same way the
  // clipboard and menu overlays let go of the screen.
  property real reveal: active ? 1 : 0
  Behavior on reveal {
    NumberAnimation {
      duration: Style.duration(150)
      easing.type: Easing.Bezier
      easing.bezierCurve: Style.easeOut
    }
  }

  // A stopped pulse animation keeps the last value it wrote; put the dot back
  // the moment the run ends instead of leaving it at whatever alpha.
  onBusyChanged: if (!busy) statusDot.opacity = 1

  // Clicking anywhere on the card is a request for the keyboard, which matters
  // exactly when an action took it away.
  MouseArea {
    anchors.fill: parent
    z: -1
    onClicked: if (root.actingFocus) root.actingFocusRelease()
  }

  // Anything with focus that does not use Escape itself bubbles it here. Without
  // this, Escape only worked while a sheet was open, and a card whose focus
  // landed nowhere could not be closed from the keyboard at all.
  Keys.onPressed: function(event) {
    if (event.key !== Qt.Key_Escape) return
    if (root.lookConsentVisible || root.brokerConfirm || root.actionConfirm) return
    if (root.settingsOpen || root.permissionsOpen || root.costsOpen) return
    if (root.welcomeOpen) root.welcomeClosed()
    else root.dismissed()
    event.accepted = true
  }

  onActionStateChanged: {
    if (root.settingsOpen || root.permissionsOpen || root.costsOpen) return
    if (root.actionConfirm) Qt.callLater(function() { actionRow.forceActiveFocus() })
    else if (!root.actionMode && !root.brokerConfirm) Qt.callLater(function() { root.focusComposer() })
  }

  onBrokerConfirmChanged: {
    // Show the pixels before the approval, not after the answer.
    if (root.brokerConfirm) root.previewRequested()
    if (root.settingsOpen || root.permissionsOpen || root.costsOpen) return
    if (root.brokerConfirm) Qt.callLater(function() { brokerRow.forceActiveFocus() })
    else if (!root.actionMode) Qt.callLater(function() { root.focusComposer() })
  }

  onSettingsOpenChanged: {
    if (root.settingsOpen) Qt.callLater(function() {
      if (settingsLoader.item) settingsLoader.item.forceActiveFocus()
    })
    else if (!root.permissionsOpen && !root.costsOpen) Qt.callLater(function() { root.focusComposer() })
  }

  onPermissionsOpenChanged: {
    if (root.permissionsOpen) Qt.callLater(function() {
      if (permissionsLoader.item) permissionsLoader.item.forceActiveFocus()
    })
    else if (!root.settingsOpen && !root.costsOpen) Qt.callLater(function() { root.focusComposer() })
  }

  onCostsOpenChanged: {
    if (root.costsOpen) Qt.callLater(function() {
      if (costsLoader.item) costsLoader.item.forceActiveFocus()
    })
    else if (!root.settingsOpen && !root.permissionsOpen) Qt.callLater(function() { root.focusComposer() })
  }

  onLookStateChanged: {
    if (root.settingsOpen || root.permissionsOpen || root.costsOpen) return
    if (root.consentMode) Qt.callLater(function() { consentRow.forceActiveFocus() })
    else if (root.looking) Qt.callLater(function() { lookingRow.forceActiveFocus() })
    else if (root.lookState === "") Qt.callLater(function() { root.focusComposer() })
  }

  // Follow the answer while it streams; a person scrolling up takes over.
  property bool follow: true
  onActiveChanged: if (active) {
    follow = true
    Qt.callLater(function() {
      root.focusComposer()
      root.scrollToEnd()
    })
  }

  onSetupModeChanged: {
    if (root.settingsOpen || root.permissionsOpen || root.costsOpen) return
    Qt.callLater(function() { root.focusComposer() })
  }

  function focusComposer() {
    if (root.actingFocus) {
      root.actingFocusRelease()
      return
    }
    if (root.active && !root.settingsOpen && !root.permissionsOpen && !root.costsOpen) {
      if (root.setupMode) setupPane.forceActiveFocus()
      else input.forceActiveFocus()
    }
  }

  // Up in an empty composer is "what did I just ask", like a shell. With text in
  // the field it keeps scrolling the transcript, so reading never loses the
  // draft.
  function recallPrevious() {
    var list = root.sentHistory || []
    if (!list.length) return false
    if (root.recallIndex < 0) {
      root.recallDraft = input.text
      root.recallIndex = list.length - 1
    } else if (root.recallIndex > 0) {
      root.recallIndex -= 1
    } else {
      return false
    }
    input.text = String(list[root.recallIndex] || "")
    input.cursorPosition = input.text.length
    return true
  }

  function recallNext() {
    var list = root.sentHistory || []
    if (root.recallIndex < 0) return false
    root.recallIndex += 1
    if (root.recallIndex >= list.length) {
      root.recallIndex = -1
      input.text = root.recallDraft
    } else {
      input.text = String(list[root.recallIndex] || "")
    }
    input.cursorPosition = input.text.length
    return true
  }

  function setDraft(text) {
    input.text = String(text)
    input.cursorPosition = input.text.length
    root.focusComposer()
  }

  function sendDraft() {
    root.recallIndex = -1
    root.recallDraft = ""
    if (root.historyMode) return
    var text = String(input.text || "").replace(/\s+$/, "")
    if (!text.length) return
    if (!root.historyReady) {
      root.nudge = true
      nudgeTimer.restart()
      return
    }
    if (root.busy) {
      // One answer at a time: say "not yet" with the input's own accent rather
      // than swallowing the key press, and keep the text for afterwards.
      root.nudge = true
      nudgeTimer.restart()
      return
    }
    input.text = ""
    root.follow = true
    root.submitted(text)
  }

  property bool nudge: false

  function scrollToEnd() {
    var go = function() {
      scroller.contentY = Math.max(0, scroller.contentHeight - scroller.height)
    }
    go()
    // A delegate that just grew reports its height on the next turn; without this
    // the last line stays half under the composer while an answer streams in.
    Qt.callLater(go)
  }

  function scrollBy(amount) {
    var limit = Math.max(0, scroller.contentHeight - scroller.height)
    scroller.contentY = Math.max(0, Math.min(limit, scroller.contentY + amount))
  }

  // --- conversation history ------------------------------------------------

  function openHistory() {
    if (!root.historyModel) return
    root.historyMode = true
    root.historyIndex = root.indexOfCurrentSession()
    Qt.callLater(function() { historyList.positionViewAtIndex(root.historyIndex, ListView.Contain) })
  }

  function closeHistory() {
    root.historyMode = false
    root.focusComposer()
  }

  function indexOfCurrentSession() {
    var model = root.historyModel
    if (!model) return 0
    for (var i = 0; i < model.count; i++) {
      if (String(model.get(i).file) === root.currentSession) return i
    }
    return 0
  }

  function moveHistory(delta) {
    var model = root.historyModel
    if (!model || model.count === 0) return
    root.historyIndex = (root.historyIndex + delta + model.count) % model.count
  }

  function chooseHistory() {
    var model = root.historyModel
    if (!model || model.count === 0) return
    var row = model.get(root.historyIndex)
    root.historyMode = false
    root.focusComposer()
    root.sessionChosen(String(row.file))
  }

  readonly property int historyRowHeight: Math.max(Style.space(40),
    Style.font.body + Style.font.caption + Style.spacing.rowPaddingX)

  Rectangle {
    id: scrim
    anchors.fill: parent
    color: Commons.Util.alpha(root.scrim, 0.25)
    opacity: root.blank ? 0 : root.reveal
  }

  MouseArea {
    anchors.fill: parent
    enabled: root.active
    onClicked: {
      if (root.settingsOpen) root.settingsClosed()
      else if (root.permissionsOpen) root.permissionsClosed()
      else if (root.costsOpen) root.costsClosed()
      else root.dismissed()
    }
  }

  BorderSurface {
    id: card
    x: Math.round(root.width - root.cardWidth - Style.space(16))
    y: Math.round(root.topMargin)
    width: root.cardWidth
    height: root.cardHeight
    // The theme decides the card's opacity (its menu surface); multiplying it
    // down here is what made the chat translucent over a busy window.
    color: root.surface
    borderSpec: root.borderSpec
    padding: root.contentMargin

    // The opening fade is skipped while blanked (a capture in progress).
    opacity: root.blank ? 0 : root.reveal
    transform: Translate { y: (1 - root.reveal) * Style.space(10) }
    scale: 0.985 + 0.015 * root.reveal

    MouseArea {
      anchors.fill: parent
      onClicked: root.focusComposer()
    }

    Item {
      id: content
      anchors.fill: parent
      anchors.topMargin: card.contentTopInset
      anchors.bottomMargin: card.contentBottomInset
      anchors.leftMargin: card.contentLeftInset
      anchors.rightMargin: card.contentRightInset

      Item {
        id: header
        anchors.top: parent.top
        anchors.left: parent.left
        anchors.right: parent.right
        height: root.headerHeight

        Text {
          id: brandMark
          anchors.left: parent.left
          anchors.verticalCenter: parent.verticalCenter
          textFormat: Text.PlainText
          text: "◆"
          color: root.accent
          font.family: root.fontFamily
          font.pixelSize: Style.font.title
        }

        Text {
          anchors.left: brandMark.right
          anchors.leftMargin: Style.space(9)
          anchors.verticalCenter: parent.verticalCenter
          textFormat: Text.PlainText
          text: "OMASSISTANT"
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
          font.bold: true
          font.letterSpacing: 1.6
        }

        // The same control height as the state pill beside it: STOP was a bare
        // 18px word next to a bordered pill, which read as two different toolbars.
        BorderSurface {
          id: stopButton
          anchors.right: settingsButton.left
          anchors.rightMargin: Style.space(10)
          anchors.verticalCenter: parent.verticalCenter
          implicitWidth: Math.max(Style.space(38), stopText.implicitWidth
            + Style.spacing.controlPaddingX * 2)
          implicitHeight: stopText.implicitHeight + Style.spacing.controlPaddingY * 2
          visible: root.busy
          color: stopMouse.containsMouse
            ? Commons.Util.alpha(root.urgent, 0.16) : "transparent"
          borderSpec: Border.controlSpec("normal", root.foreground, root.accent, root.urgent)
          radius: root.cornerRadius
          Behavior on color { ColorAnimation { duration: Style.duration(120) } }

          Text {
            id: stopText
            anchors.centerIn: parent
            textFormat: Text.PlainText
            text: "STOP"
            color: stopMouse.containsMouse ? root.urgent : root.accent
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            font.bold: true
            font.letterSpacing: 1.0
          }
          MouseArea {
            id: stopMouse
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: root.stopRequested()
          }
        }

        Item {
          id: settingsButton
          anchors.right: statePill.left
          anchors.rightMargin: Style.space(10)
          anchors.verticalCenter: parent.verticalCenter
          width: Math.max(Style.space(18), Style.font.bodySmall + Style.space(4))
          height: width
          visible: !root.settingsOpen
          Text {
            anchors.centerIn: parent
            textFormat: Text.PlainText
            text: "󰒓"
            color: settingsMouse.containsMouse ? root.accent : root.textTertiary
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
          }
          MouseArea {
            id: settingsMouse
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: root.settingsRequested()
          }
        }

        // The permission state, as a control pill rather than dim caption text:
        // it is the product's most important sentence, so it has to be legible.
        BorderSurface {
          id: statePill
          anchors.right: parent.right
          anchors.verticalCenter: parent.verticalCenter
          implicitWidth: stateRow.implicitWidth + Style.spacing.controlPaddingX * 2
          implicitHeight: stateRow.implicitHeight + Style.spacing.controlPaddingY * 2
          color: "transparent"
          borderSpec: Border.controlSpec(root.runtimeFault.length > 0 ? "focus" : "normal",
            root.foreground, root.accent, root.urgent)
          radius: root.cornerRadius

          Row {
            id: stateRow
            anchors.centerIn: parent
            spacing: Style.space(7)

            // Filled while the runtime is working (or broken); hollow when it
            // is idle with nothing granted, the state it lives in.
            Rectangle {
              id: statusDot
              anchors.verticalCenter: parent.verticalCenter
              width: Style.space(5)
              height: width
              radius: width / 2
              color: root.statusHollow ? "transparent" : root.statusColor
              border.width: root.statusHollow ? Math.max(1, Style.space(1)) : 0
              border.color: root.statusColor

              SequentialAnimation on opacity {
                id: pulse
                running: root.busy && !Style.reduceMotion
                loops: Animation.Infinite
                NumberAnimation { from: 1; to: 0.2; duration: 640; easing.type: Easing.InOutSine }
                NumberAnimation { from: 0.2; to: 1; duration: 640; easing.type: Easing.InOutSine }
              }
              // A stopped SequentialAnimation keeps the last value it wrote;
              // the root resets it when the run ends (see onBusyChanged).
            }

            Text {
              anchors.verticalCenter: parent.verticalCenter
              textFormat: Text.PlainText
              text: root.statusLabel
              color: root.statusColor
              font.family: root.fontFamily
              font.pixelSize: Style.font.body
              font.bold: true
              font.letterSpacing: 1.2
            }
          }

          MouseArea {
            anchors.fill: parent
            cursorShape: Qt.PointingHandCursor
            onClicked: root.permissionsRequested()
          }
        }
      }

      Item {
        id: body
        anchors.top: header.bottom
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: composer.top
        anchors.topMargin: root.sectionGap
        anchors.bottomMargin: root.sectionGap
        clip: true

        // A startup failure is not an empty chat with a red word. This is the
        // one place where the card explains a required external thing (pi) and
        // gives the person the command, the link and a retry, without silently
        // installing an agent runtime on their behalf.
        Item {
          id: setupPane
          objectName: "omassistantSetup"
          anchors.fill: parent
          visible: root.setupMode
          focus: visible

          Keys.onPressed: function(event) {
            if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter
                || event.key === Qt.Key_R) {
              root.setupRetry()
              event.accepted = true
            } else if (event.key === Qt.Key_C) {
              root.setupCopy()
              event.accepted = true
            } else if (event.key === Qt.Key_O) {
              root.setupOpen()
              event.accepted = true
            } else if (event.key === Qt.Key_S) {
              root.settingsRequested()
              event.accepted = true
            } else if (event.key === Qt.Key_Escape) {
              root.dismissed()
              event.accepted = true
            }
          }

          Column {
            x: root.gutter
            width: parent.width - root.gutter * 2
            anchors.top: parent.top
            anchors.topMargin: Style.space(4)
            spacing: Style.space(11)

            Text {
              textFormat: Text.PlainText
              text: "SETUP"
              color: root.accent
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              font.bold: true
              font.letterSpacing: 1.6
            }

            Text {
              width: parent.width
              textFormat: Text.PlainText
              text: root.setupTitle
              wrapMode: Text.Wrap
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.heading
            }

            Text {
              width: parent.width
              textFormat: Text.PlainText
              text: root.setupBody
              wrapMode: Text.Wrap
              color: root.textSecondary
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
              lineHeightMode: Text.FixedHeight
              lineHeight: Style.space(17)
            }

            Text {
              visible: root.setupMissing
              textFormat: Text.PlainText
              text: "ON OMARCHY  ·  MISE"
              color: root.textTertiary
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              font.bold: true
              font.letterSpacing: 1.2
            }

            Rectangle {
              visible: root.setupMissing
              width: parent.width
              height: setupCommandText.implicitHeight + Style.spacing.controlPaddingY * 2
              color: "transparent"
              border.width: Math.max(1, Style.space(1))
              border.color: Commons.Util.alpha(root.border, 0.85)
              radius: root.cornerRadius

              Text {
                id: setupCommandText
                anchors.left: parent.left
                anchors.leftMargin: Style.spacing.controlPaddingX
                anchors.verticalCenter: parent.verticalCenter
                textFormat: Text.PlainText
                text: "pi --version"
                color: root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.body
              }

              Text {
                id: setupCopyText
                anchors.right: parent.right
                anchors.rightMargin: Style.spacing.controlPaddingX
                anchors.verticalCenter: parent.verticalCenter
                textFormat: Text.PlainText
                text: "COPY"
                color: setupCopyMouse.containsMouse ? root.accent : root.textTertiary
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                font.bold: true
                font.letterSpacing: 1.0

                MouseArea {
                  id: setupCopyMouse
                  anchors.fill: parent
                  anchors.margins: -Style.space(6)
                  hoverEnabled: true
                  cursorShape: Qt.PointingHandCursor
                  onClicked: root.setupCopy()
                }
              }
            }

            Text {
              visible: root.setupMissing
              width: parent.width
              textFormat: Text.PlainText
              text: "Then run `pi` once in a terminal to sign in, and choose a model in Settings."
              wrapMode: Text.Wrap
              color: root.textSecondary
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
              lineHeightMode: Text.FixedHeight
              lineHeight: Style.space(17)
            }

            Text {
              width: parent.width
              visible: root.runtimeFault.length > 0
              textFormat: Text.PlainText
              text: root.runtimeFault
              wrapMode: Text.Wrap
              color: root.urgent
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              lineHeightMode: Text.FixedHeight
              lineHeight: Style.space(14)
            }

            Row {
              spacing: Style.space(18)

              Text {
                textFormat: Text.PlainText
                text: "RETRY"
                color: root.accent
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                font.bold: true
                font.letterSpacing: 1.0

                MouseArea {
                  id: setupRetryMouse
                  anchors.fill: parent
                  anchors.margins: -Style.space(6)
                  hoverEnabled: true
                  cursorShape: Qt.PointingHandCursor
                  onClicked: root.setupRetry()
                }
              }

              Text {
                visible: root.setupMissing
                textFormat: Text.PlainText
                text: "OPEN PI.DEV"
                color: setupOpenMouse.containsMouse ? root.accent : root.textSecondary
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                font.bold: true
                font.letterSpacing: 1.0

                MouseArea {
                  id: setupOpenMouse
                  anchors.fill: parent
                  anchors.margins: -Style.space(6)
                  hoverEnabled: true
                  cursorShape: Qt.PointingHandCursor
                  onClicked: root.setupOpen()
                }
              }

              Text {
                textFormat: Text.PlainText
                text: "SETTINGS"
                color: setupSettingsMouse.containsMouse ? root.accent : root.textSecondary
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                font.bold: true
                font.letterSpacing: 1.0

                MouseArea {
                  id: setupSettingsMouse
                  anchors.fill: parent
                  anchors.margins: -Style.space(6)
                  hoverEnabled: true
                  cursorShape: Qt.PointingHandCursor
                  onClicked: root.settingsRequested()
                }
              }
            }
          }
        }

        // The empty state is a line of copy, not a splash page: the brand is in
        // the header, and the input says "ask anything" already.
        Column {
          width: parent.width - root.gutter
          x: root.gutter
          anchors.verticalCenter: parent.verticalCenter
          visible: root.bodyContentHeight === 0 && !root.historyMode && root.historyReady && !root.setupMode
          spacing: Style.space(6)

          Text {
            textFormat: Text.PlainText
            text: "Ask about this desktop."
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.heading
          }

          Text {
            width: parent.width
            wrapMode: Text.Wrap
            textFormat: Text.PlainText
            text: "Look at what is on screen, open an app, run a command, act in a window — each one after you allow it."
            color: root.textSecondary
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
            lineHeight: 1.35
          }

          // Concrete starters. A blank card asks the person to invent the first
          // question; these are the ones that show what the thing is for.
          Repeater {
            // The first three things to try, in the order they earn trust: look at
            // what is already in front of you, change something small, then ask a
            // question only this machine can answer.
            model: [
              "Explain the error on my screen.",
              "Tidy my Downloads into folders by type.",
              "What is the biggest thing in my home folder?"
            ]

            delegate: Item {
              required property int index
              required property string modelData
              width: parent.width
              height: starterText.implicitHeight + Style.space(6)

              Text {
                id: starterText
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                textFormat: Text.PlainText
                text: "›  " + parent.modelData
                color: starterMouse.containsMouse ? root.accent : root.textSecondary
                font.family: root.fontFamily
                font.pixelSize: Style.font.bodySmall
                elide: Text.ElideRight
                Behavior on color { ColorAnimation { duration: Style.duration(120) } }
              }

              MouseArea {
                id: starterMouse
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: root.submitted(parent.modelData)
              }
            }
          }

          Text {
            width: parent.width
            visible: root.runtimeStarting
            wrapMode: Text.Wrap
            textFormat: Text.PlainText
            text: "Starting pi… first run may install it through Omarchy's mise."
            color: root.textTertiary
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
          }

          // Air between the starters and the links: they are two different kinds
          // of thing, and six pixels said they were one list.
          Item { width: 1; height: Style.space(6) }

          Row {
            spacing: Style.space(18)

            Text {
              textFormat: Text.PlainText
              text: "LOOK AT SCREEN"
              color: lookEmptyMouse.containsMouse ? root.accent : root.textSecondary
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              font.bold: true
              font.letterSpacing: 1.0
              MouseArea {
                id: lookEmptyMouse
                anchors.fill: parent
                anchors.margins: -Style.space(4)
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: root.lookRequested("")
              }
            }
            Text {
              textFormat: Text.PlainText
              text: "PERMISSIONS"
              color: permsEmptyMouse.containsMouse ? root.accent : root.textSecondary
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              font.bold: true
              font.letterSpacing: 1.0
              MouseArea {
                id: permsEmptyMouse
                anchors.fill: parent
                anchors.margins: -Style.space(4)
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: root.permissionsRequested()
              }
            }
            Text {
              textFormat: Text.PlainText
              text: "SETTINGS"
              color: settingsEmptyMouse.containsMouse ? root.accent : root.textSecondary
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              font.bold: true
              font.letterSpacing: 1.0
              MouseArea {
                id: settingsEmptyMouse
                anchors.fill: parent
                anchors.margins: -Style.space(4)
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: root.settingsRequested()
              }
            }
          }
        }

        Flickable {
          id: scroller
          anchors.fill: parent
          // The look preview sits in the gap above the composer; the transcript
          // yields its space instead of being drawn over.
          anchors.bottomMargin: previewStrip.visible ? previewStrip.height + Style.space(8) : 0
          visible: root.bodyContentHeight > 0 && !root.historyMode && !root.setupMode
          clip: true
          boundsBehavior: Flickable.StopAtBounds
          contentWidth: width
          contentHeight: transcriptColumn.implicitHeight
          interactive: false
          onContentHeightChanged: if (root.follow) root.scrollToEnd()
          onHeightChanged: if (root.follow) root.scrollToEnd()

          Column {
            id: transcriptColumn
            width: scroller.width
            spacing: root.messageGap

            // A little air under the last line, so "scrolled to the end" does not
            // leave a clipped half-line against the composer.
            // Only when there is something above it: an empty column with a spacer
            // is "content", which hid the empty state.
            Item {
              width: 1
              height: root.transcript && root.transcript.count > 0 ? root.sectionGap : 0
            }

            Repeater {
              model: root.transcript

              delegate: Item {
                id: row
                required property int index
                required property string kind
                required property string label
                required property string text
                required property bool streaming
                required property bool failed

                readonly property bool isQuestion: kind === "you"
                readonly property bool isNote: kind === "note"
                readonly property real topGap: isQuestion && index > 0 ? root.turnGap : 0

                width: transcriptColumn.width
                height: row.topGap + (row.isQuestion ? bubble.height : (row.isNote ? note.height : answer.height))

                // A question is the shell's "chosen row" idiom: a filled row at
                // the content edge, text on the shared column.
                Rectangle {
                  id: bubble
                  visible: row.isQuestion
                  y: row.topGap
                  width: parent.width
                  height: visible ? youText.height + Style.spacing.controlPaddingY * 2 : 0
                  color: root.questionFill
                  radius: root.cornerRadius

                  Text {
                    id: youText
                    x: root.gutter
                    y: Style.spacing.controlPaddingY
                    width: parent.width - root.gutter - Style.spacing.rowPaddingX
                    textFormat: Text.PlainText
                    text: row.text
                    wrapMode: Text.Wrap
                    color: root.questionText
                    font.family: root.fontFamily
                    font.pixelSize: root.bodyFontSize
                    lineHeightMode: Text.FixedHeight
                    lineHeight: root.questionLeading
                  }
                }

                Text {
                  id: answer
                  visible: !row.isQuestion && !row.isNote
                  y: row.topGap
                  x: root.gutter
                  width: parent.width - root.gutter
                  textFormat: Text.PlainText
                  // The trailing space keeps the wrap stable when the block
                  // cursor leaves at the end of a stream; see streaming below.
                  text: row.text + (row.streaming ? "▍" : " ")
                  wrapMode: Text.Wrap
                  color: root.foreground
                  font.family: root.fontFamily
                  font.pixelSize: root.bodyFontSize
                  lineHeightMode: Text.FixedHeight
                  lineHeight: root.bodyLeading
                }

                Item {
                  id: note
                  visible: row.isNote
                  y: row.topGap
                  width: parent.width
                  height: noteBody.height

                  Rectangle {
                    id: noteBar
                    width: Math.max(2, Style.space(2))
                    height: parent.height
                    color: row.failed ? root.urgent : root.textTertiary
                  }

                  Column {
                    id: noteBody
                    x: Style.space(10)
                    width: parent.width - x
                    spacing: Style.space(2)

                    Text {
                      textFormat: Text.PlainText
                      visible: row.label.length > 0
                      text: row.label
                      color: row.failed ? root.urgent : root.textSecondary
                      font.family: root.fontFamily
                      font.pixelSize: Style.font.caption
                      font.bold: true
                      font.letterSpacing: 1.2
                    }

                    Text {
                      width: parent.width
                      textFormat: Text.PlainText
                      text: row.text
                      wrapMode: Text.Wrap
                      color: root.textSecondary
                      font.family: root.fontFamily
                      font.pixelSize: Style.font.bodySmall
                      lineHeightMode: Text.FixedHeight
                      lineHeight: Style.space(14)
                    }
                  }
                }
              }
            }
          }
        }

        // Fades mark that the transcript continues past the edge, so a long
        // answer dissolves instead of being sliced through a line of text.
        Rectangle {
          anchors.top: parent.top
          anchors.left: parent.left
          anchors.right: parent.right
          height: Style.space(16)
          visible: !root.setupMode && !root.historyMode && scroller.contentY > 1
          gradient: Gradient {
            GradientStop { position: 0.0; color: root.surface }
            GradientStop { position: 1.0; color: "transparent" }
          }
        }

        Rectangle {
          anchors.bottom: parent.bottom
          anchors.left: parent.left
          anchors.right: parent.right
          height: Style.space(16)
          visible: !root.setupMode && !root.historyMode && scroller.contentY + scroller.height < scroller.contentHeight - 1
          gradient: Gradient {
            GradientStop { position: 0.0; color: "transparent" }
            GradientStop { position: 1.0; color: root.surface }
          }
        }

        // Restoring a conversation is a short, quiet state; say so rather than
        // showing an empty card that looks like lost history.
        Text {
          anchors.top: parent.top
          anchors.left: parent.left
          visible: !root.historyReady && !root.historyMode && !root.setupMode
          textFormat: Text.PlainText
          text: "restoring conversation…"
          color: root.textTertiary
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
        }

        // The history picker reuses the card: one list in the menu's row idiom,
        // with the live session marked. It is a mode, not a second window.
        ListView {
          id: historyList
          anchors.fill: parent
          visible: root.historyMode && !root.setupMode
          clip: true
          model: root.historyModel
          spacing: Style.space(2)
          boundsBehavior: Flickable.StopAtBounds
          onCountChanged: if (root.historyIndex >= count) root.historyIndex = Math.max(0, count - 1)

          delegate: Rectangle {
            id: historyRow
            required property int index
            required property string title
            required property string when
            required property int turns
            required property bool current

            readonly property bool selected: index === root.historyIndex

            width: ListView.view ? ListView.view.width : 0
            height: root.historyRowHeight
            color: selected ? root.questionFill : "transparent"

            Behavior on color { ColorAnimation { duration: Style.duration(120) } }

            Text {
              id: meta
              anchors.right: parent.right
              anchors.rightMargin: Style.spacing.rowPaddingX
              anchors.verticalCenter: parent.verticalCenter
              // A fixed meta column, right-aligned: right-aligning by content made
              // the "when · turns" column start at a different x on every row.
              width: Math.round((parent.width - Style.spacing.rowPaddingX * 2) * 0.46)
              horizontalAlignment: Text.AlignRight
              elide: Text.ElideLeft
              textFormat: Text.PlainText
              text: (historyRow.current ? "now  ·  " : "") + historyRow.when
                + (historyRow.turns > 0 ? "  ·  " + historyRow.turns + (historyRow.turns === 1 ? " turn" : " turns") : "")
              color: historyRow.selected ? root.accent : root.textTertiary
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              font.letterSpacing: 0.4
            }

            Text {
              anchors.left: parent.left
              anchors.leftMargin: Style.spacing.rowPaddingX
              anchors.right: meta.left
              anchors.rightMargin: Style.space(10)
              anchors.verticalCenter: parent.verticalCenter
              textFormat: Text.PlainText
              text: historyRow.title
              elide: Text.ElideRight
              color: historyRow.selected ? root.accent : root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.body
            }

            MouseArea {
              anchors.fill: parent
              onClicked: {
                root.historyIndex = historyRow.index
                root.chooseHistory()
              }
            }
          }
        }

        Text {
          anchors.top: parent.top
          anchors.left: parent.left
          visible: root.historyMode && !root.setupMode && (!root.historyModel || root.historyModel.count === 0)
          textFormat: Text.PlainText
          text: "no earlier conversations"
          color: root.textTertiary
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
        }
      }

      PanelSeparator {
        id: composerRule
        visible: !root.setupMode
        anchors.bottom: footer.top
        anchors.bottomMargin: root.sectionGap
        anchors.left: parent.left
        anchors.right: parent.right
        foreground: root.nudge ? root.urgent : (input.activeFocus ? root.accent : root.foreground)
        // The input's only affordance: present at rest, accent when focused,
        // a brief urgent flash when Enter arrives mid-answer.
        strength: root.nudge ? 0.8 : (input.activeFocus ? 0.55 : 0.28)
        Behavior on strength { NumberAnimation { duration: Style.duration(140) } }
        Behavior on foreground { ColorAnimation { duration: Style.duration(140) } }
      }

      Item {
        id: composer
        visible: !root.setupMode
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: composerRule.top
        anchors.bottomMargin: Style.spacing.md
        height: root.setupMode ? 0 : root.composerHeight

        // The run as a workflow. A person watching a GUI agent sees steps happen;
        // this shows the same thing, and the header says how many of them were
        // clicks — usually none, because a command did the work.
        Item {
          id: stepsStrip
          anchors.left: parent.left
          anchors.right: parent.right
          anchors.bottom: parent.top
          anchors.bottomMargin: Style.space(10)
          height: visible ? stepColumn.implicitHeight + Style.space(4) : 0
          visible: root.busy && !root.blank && root.runSteps.length > 0

          Column {
            id: stepColumn
            width: parent.width
            spacing: Style.space(2)

            Text {
              textFormat: Text.PlainText
              text: root.runSteps.length + (root.runSteps.length === 1 ? " STEP" : " STEPS")
                + "  ·  " + root.runClicks + (root.runClicks === 1 ? " CLICK" : " CLICKS")
              color: root.textTertiary
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              font.bold: true
              font.letterSpacing: 1.0
            }

            Repeater {
              model: root.runSteps
              delegate: Text {
                required property int index
                required property var modelData
                textFormat: Text.PlainText
                width: stepColumn.width
                elide: Text.ElideRight
                text: (index === root.runSteps.length - 1 ? "▸ " : "  ") + modelData.label
                color: index === root.runSteps.length - 1 ? root.accent : root.textSecondary
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
              }
            }
          }
        }

        // Where the last answer came from: the search returned these with it.
        // Small enough to be a footnote, real enough to be worth showing.
        Item {
          id: mediaStrip
          anchors.left: parent.left
          anchors.right: parent.right
          anchors.bottom: parent.bottom
          anchors.bottomMargin: -Style.space(4)
          height: visible ? Style.space(30) : 0
          visible: !root.blank && !root.busy && root.mediaImages.length > 0
            && !root.brokerConfirm && !root.lookConsentVisible

          Row {
            anchors.left: parent.left
            anchors.verticalCenter: parent.verticalCenter
            spacing: Style.space(6)

            Repeater {
              model: Math.min(root.mediaImages.length, 4)
              delegate: Rectangle {
                width: Style.space(22)
                height: width
                radius: Style.space(3)
                color: "transparent"
                border.color: Commons.Util.alpha(root.foreground, 0.18)
                border.width: 1
                clip: true

                Image {
                  anchors.fill: parent
                  anchors.margins: 2
                  asynchronous: true
                  fillMode: Image.PreserveAspectFit
                  source: root.mediaImages[index] ? String(root.mediaImages[index].url || "") : ""
                }
              }
            }
          }

          Text {
            anchors.left: parent.left
            anchors.leftMargin: Math.min(root.mediaImages.length, 4) * Style.space(28)
            anchors.verticalCenter: parent.verticalCenter
            textFormat: Text.PlainText
            text: "FROM THE SEARCH"
            color: root.textTertiary
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            font.letterSpacing: 0.8
          }
        }

        // What the model is about to look at, above the question about looking.
        Item {
          id: previewStrip
          anchors.left: parent.left
          anchors.right: parent.right
          anchors.bottom: parent.top
          anchors.bottomMargin: Style.space(8)
          height: visible ? Style.space(140) : 0
          visible: root.brokerConfirm && root.previewReady && !root.blank

          Rectangle {
            id: previewFrame
            anchors.left: parent.left
            anchors.top: parent.top
            width: Style.space(240)
            height: parent.height
            color: "transparent"
            border.color: Commons.Util.alpha(root.foreground, 0.16)
            border.width: 1
            radius: Style.space(4)
            clip: true

            Image {
              id: previewImage
              anchors.fill: parent
              anchors.margins: 1
              fillMode: Image.PreserveAspectCrop
              asynchronous: true
              cache: false
              source: root.previewUrl
            }
          }

          Text {
            anchors.left: previewFrame.right
            anchors.leftMargin: Style.space(12)
            anchors.right: parent.right
            anchors.top: previewFrame.top
            textFormat: Text.PlainText
            text: "WHAT IT WILL SEE\n" + root.previewCaption
            color: root.textTertiary
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            font.letterSpacing: 0.6
            lineHeight: 1.4
            wrapMode: Text.Wrap
            elide: Text.ElideRight
            maximumLineCount: 3
          }
        }

        // Consent is a mode of the card, not a dialog: one row, one decision,
        // the same keys as everything else (Enter allows, Esc backs out).
        Item {
          id: consentRow
          anchors.fill: parent
          z: 1
          // Its own surface: the row shares the composer's slot, and the
          // composer's controls must not show through the question.
          Rectangle { anchors.fill: parent; color: root.surface; z: -1 }
          visible: root.lookConsentVisible
          focus: visible

          Keys.onPressed: function(event) {
            if (event.key === Qt.Key_Escape) {
              root.lookCancelled()
              event.accepted = true
            } else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
              root.lookAllowed()
              event.accepted = true
            } else if (event.key === Qt.Key_P && (event.modifiers & Qt.ControlModifier)) {
              root.permissionsRequested()
              event.accepted = true
            }
          }

          Text {
            id: consentGlyph
            anchors.left: parent.left
            anchors.verticalCenter: parent.verticalCenter
            textFormat: Text.PlainText
            text: "◇"
            color: root.accent
            font.family: root.fontFamily
            font.pixelSize: Style.font.heading
          }

          Text {
            id: consentLabel
            anchors.left: consentGlyph.right
            anchors.leftMargin: Style.space(10)
            anchors.verticalCenter: parent.verticalCenter
            textFormat: Text.PlainText
            text: "LOOK AT YOUR SCREEN?"
            color: root.accent
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            font.bold: true
            font.letterSpacing: 1.2
          }

          Text {
            anchors.left: consentLabel.right
            anchors.leftMargin: Style.space(12)
            anchors.right: consentKeys.left
            anchors.rightMargin: Style.space(12)
            anchors.verticalCenter: parent.verticalCenter
            textFormat: Text.PlainText
            text: root.consentWindow.length ? root.consentWindow : "this desktop"
            color: root.textSecondary
            elide: Text.ElideRight
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
          }

          Text {
            id: consentKeys
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            textFormat: Text.PlainText
            text: "ENTER ALLOW  ·  CTRL+P PERMISSIONS  ·  ESC"
            color: root.textTertiary
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            font.letterSpacing: 1.0
          }
        }

        Item {
          id: brokerRow
          anchors.fill: parent
          z: 3
          // Its own surface: the row shares the composer's slot, and the
          // composer's controls must not show through the question.
          Rectangle { anchors.fill: parent; color: root.surface; z: -1 }
          visible: root.brokerConfirm
          focus: visible

          Keys.onPressed: function(event) {
            if (event.key === Qt.Key_Escape) {
              root.brokerDenied()
              event.accepted = true
            } else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
              root.brokerAllowed()
              event.accepted = true
            } else if (event.key === Qt.Key_C && (event.modifiers & Qt.ControlModifier)) {
              root.brokerDenied()
              root.aborted()
              event.accepted = true
            }
          }

          Text {
            anchors.left: parent.left
            anchors.verticalCenter: parent.verticalCenter
            textFormat: Text.PlainText
            text: "◇"
            color: root.accent
            font.family: root.fontFamily
            font.pixelSize: Style.font.heading
          }

          Text {
            anchors.left: parent.left
            anchors.leftMargin: Style.space(22)
            anchors.right: brokerKeys.left
            anchors.rightMargin: Style.space(12)
            anchors.verticalCenter: parent.verticalCenter
            textFormat: Text.PlainText
            // The whole operation, wrapped. An ellipsis here would mean approving
            // characters nobody saw, which is exactly what the review called out;
            // the broker caps every operation at what these lines can hold.
            text: root.brokerSummary
            color: root.accent
            wrapMode: Text.Wrap
            maximumLineCount: 5
            elide: Text.ElideNone
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            font.bold: true
          }

          Text {
            id: brokerKeys
            visible: root.brokerSummaryLines === 1
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            textFormat: Text.PlainText
            text: "ENTER ALLOW  ·  ESC DENY"
            color: root.textTertiary
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            font.letterSpacing: 1.0
          }
        }

        Item {
          id: actionRow
          anchors.fill: parent
          z: 2
          // Its own surface: the row shares the composer's slot, and the
          // composer's controls must not show through the question.
          Rectangle { anchors.fill: parent; color: root.surface; z: -1 }
          visible: root.actionConfirmVisible
          focus: visible

          Keys.onPressed: function(event) {
            if (event.key === Qt.Key_Escape) {
              root.actionCancelled()
              event.accepted = true
            } else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
              root.actionAllowed()
              event.accepted = true
            }
          }

          Text {
            anchors.left: parent.left
            anchors.verticalCenter: parent.verticalCenter
            textFormat: Text.PlainText
            text: "◇"
            color: root.accent
            font.family: root.fontFamily
            font.pixelSize: Style.font.heading
          }

          Text {
            anchors.left: parent.left
            anchors.leftMargin: Style.space(22)
            anchors.right: actionKeys.left
            anchors.rightMargin: Style.space(12)
            anchors.verticalCenter: parent.verticalCenter
            textFormat: Text.PlainText
            text: root.actionTitle
            color: root.accent
            elide: Text.ElideRight
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            font.bold: true
            font.letterSpacing: 1.0
          }

          Text {
            id: actionKeys
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            textFormat: Text.PlainText
            text: "ENTER ALLOW  ·  ESC CANCEL"
            color: root.textTertiary
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            font.letterSpacing: 1.0
          }
        }

        Item {
          id: actionBusyRow
          anchors.fill: parent
          visible: root.actionRunning

          Text {
            anchors.left: parent.left
            anchors.verticalCenter: parent.verticalCenter
            textFormat: Text.PlainText
            text: "◇"
            color: root.accent
            font.family: root.fontFamily
            font.pixelSize: Style.font.heading

            SequentialAnimation on opacity {
              running: root.actionRunning && !Style.reduceMotion
              loops: Animation.Infinite
              NumberAnimation { from: 1; to: 0.2; duration: 480; easing.type: Easing.InOutSine }
              NumberAnimation { from: 0.2; to: 1; duration: 480; easing.type: Easing.InOutSine }
            }
          }

          Text {
            anchors.left: parent.left
            anchors.leftMargin: Style.space(22)
            anchors.verticalCenter: parent.verticalCenter
            textFormat: Text.PlainText
            text: "doing it…"
            color: root.accent
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            font.letterSpacing: 0.8
          }
        }

        Item {
          id: lookingRow
          anchors.fill: parent
          visible: root.looking
          focus: visible

          Keys.onPressed: function(event) {
            if (event.key === Qt.Key_Escape) {
              root.lookCancelled()
              event.accepted = true
            }
          }

          Text {
            anchors.left: parent.left
            anchors.verticalCenter: parent.verticalCenter
            textFormat: Text.PlainText
            text: "◇"
            color: root.accent
            font.family: root.fontFamily
            font.pixelSize: Style.font.heading

            SequentialAnimation on opacity {
              running: root.looking && !Style.reduceMotion
              loops: Animation.Infinite
              NumberAnimation { from: 1; to: 0.2; duration: 600; easing.type: Easing.InOutSine }
              NumberAnimation { from: 0.2; to: 1; duration: 600; easing.type: Easing.InOutSine }
            }
          }

          Text {
            anchors.left: parent.left
            anchors.leftMargin: Style.space(22)
            anchors.verticalCenter: parent.verticalCenter
            textFormat: Text.PlainText
            text: "capturing the screen…"
            color: root.accent
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            font.letterSpacing: 0.8
          }
        }

        Text {
          id: promptGlyph
          visible: !root.anyConsent && (root.lookState === "" || root.lookState === "attached")
          anchors.left: parent.left
          anchors.verticalCenter: parent.verticalCenter
          textFormat: Text.PlainText
          text: "❯"
          color: input.activeFocus ? root.accent : root.textTertiary
          font.family: root.fontFamily
          font.pixelSize: Style.font.heading
          Behavior on color { ColorAnimation { duration: Style.duration(160) } }
        }

        // TextInput has no placeholder of its own; this is the shell's pattern
        // for one (the menu draws its prompt the same way). Declared before the
        // input so the caret paints over it rather than under it.
        Text {
          anchors.left: input.left
          anchors.right: lookButton.left
          anchors.rightMargin: Style.space(10)
          anchors.verticalCenter: parent.verticalCenter
          visible: !root.anyConsent && (root.lookState === "" || root.lookState === "attached")
            && input.text.length === 0 && input.preeditText.length === 0
          textFormat: Text.PlainText
          text: root.historyMode ? "choose a conversation"
            : root.recording ? "listening…"
            : root.transcribing ? "transcribing…"
            : root.actingFocus ? "working — click STOP to interrupt"
            : (root.lookState === "attached" ? "ask about the screen…" : (root.busy ? "" : "Ask anything"))
          color: root.textPlaceholder
          elide: Text.ElideRight
          font.family: root.fontFamily
          font.pixelSize: Style.font.heading
        }

        TextInput {
          id: input
          visible: !root.anyConsent && (root.lookState === "" || root.lookState === "attached")
          anchors.left: promptGlyph.right
          anchors.leftMargin: Style.space(10)
          anchors.right: lookButton.left
          anchors.rightMargin: Style.space(10)
          anchors.verticalCenter: parent.verticalCenter
          height: parent.height
          verticalAlignment: TextInput.AlignVCenter
          focus: true
          clip: true
          color: root.foreground
          selectionColor: Style.selectionFillFor(root.foreground, root.accent)
          selectedTextColor: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.heading
          cursorDelegate: Rectangle {
            width: Math.max(1, Style.space(2))
            color: root.accent
          }

          onAccepted: root.sendDraft()
          readOnly: root.historyMode
          enabled: !root.settingsOpen && !root.permissionsOpen && !root.costsOpen && !root.actingFocus

          Keys.onPressed: function(event) {
            if (root.brokerConfirm) {
              if (event.key === Qt.Key_Escape) root.brokerDenied()
              else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) root.brokerAllowed()
              else if (event.key === Qt.Key_C && (event.modifiers & Qt.ControlModifier)) {
                // Ctrl+C means "stop", even while the card is asking.
                root.brokerDenied()
                root.aborted()
              }
              event.accepted = true
              return
            }
            // The input usually holds focus even while a row is visible (Qt
            // keeps it until a focusable row takes over), so the mode keys are
            // handled here as well as on the row itself.
            if (root.actionConfirm) {
              if (event.key === Qt.Key_Escape) root.actionCancelled()
              else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) root.actionAllowed()
              event.accepted = true
              return
            }
            if (root.actionRunning) {
              // In flight: nothing to decide any more, and swallowing keys keeps
              // a stray Esc from closing the card mid-request.
              event.accepted = true
              return
            }
            if (root.looking) {
              if (event.key === Qt.Key_Escape) root.lookCancelled()
              event.accepted = true
              return
            }
            if (root.consentMode) {
              if (event.key === Qt.Key_Escape) root.lookCancelled()
              else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) root.lookAllowed()
              event.accepted = true
              return
            }
            if (root.historyMode) {
              if (event.key === Qt.Key_Escape) {
                root.closeHistory()
                event.accepted = true
              } else if (event.key === Qt.Key_Up) {
                root.moveHistory(-1)
                event.accepted = true
              } else if (event.key === Qt.Key_Down) {
                root.moveHistory(1)
                event.accepted = true
              } else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
                root.chooseHistory()
                event.accepted = true
              }
              return
            }
            if (event.key === Qt.Key_Comma && (event.modifiers & Qt.ControlModifier)) {
              root.settingsRequested()
              event.accepted = true
              return
            }
            if (event.key === Qt.Key_P && (event.modifiers & Qt.ControlModifier)) {
              root.permissionsRequested()
              event.accepted = true
              return
            }
            if (event.key === Qt.Key_H && (event.modifiers & Qt.ControlModifier)) {
              root.openHistory()
              event.accepted = true
              return
            }
            if (event.key === Qt.Key_D && (event.modifiers & Qt.ControlModifier)) {
              root.dictationToggled()
              event.accepted = true
              return
            }
            if (event.key === Qt.Key_O && (event.modifiers & Qt.ControlModifier)) {
              var draft = input.text
              input.text = ""
              root.lookRequested(draft)
              event.accepted = true
              return
            }
            if (event.key === Qt.Key_Escape) {
              root.dismissed()
              event.accepted = true
              return
            }
            // Ctrl+C stops a run; with no run in flight it means nothing, so
            // it never throws the conversation away by accident.
            if (event.key === Qt.Key_C && (event.modifiers & Qt.ControlModifier) && !selectedText) {
              if (root.busy) {
                root.aborted()
                event.accepted = true
              }
              return
            }
            if (event.key === Qt.Key_K && (event.modifiers & Qt.ControlModifier)) {
              root.cleared()
              event.accepted = true
              return
            }
            if (event.key === Qt.Key_Up) {
              // Empty composer: walk back through what was asked. Otherwise the
              // arrow belongs to the transcript.
              if (input.text.length === 0 || root.recallIndex >= 0) {
                if (root.recallPrevious()) {
                  event.accepted = true
                  return
                }
              }
              root.follow = false
              root.scrollBy(-Style.space(48))
              event.accepted = true
            } else if (event.key === Qt.Key_Down) {
              if (root.recallIndex >= 0) {
                root.recallNext()
                event.accepted = true
                return
              }
              root.scrollBy(Style.space(48))
              if (scroller.contentY + scroller.height >= scroller.contentHeight - 1) root.follow = true
              event.accepted = true
            } else if (event.key === Qt.Key_PageUp) {
              root.follow = false
              root.scrollBy(-scroller.height * 0.9)
              event.accepted = true
            } else if (event.key === Qt.Key_PageDown) {
              root.scrollBy(scroller.height * 0.9)
              if (scroller.contentY + scroller.height >= scroller.contentHeight - 1) root.follow = true
              event.accepted = true
            } else if (event.key === Qt.Key_End) {
              root.follow = true
              root.scrollToEnd()
              event.accepted = true
            }
          }
        }

        Text {
          id: lookButton
          anchors.right: parent.right
          anchors.verticalCenter: parent.verticalCenter
          visible: !root.confirmVisible && !root.actionRunning && !root.historyMode
            && !root.busy && root.lookState !== "looking" && root.lookState !== "consent"
          textFormat: Text.PlainText
          text: "LOOK"
          color: lookMouse.containsMouse ? root.accent : root.textTertiary
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          font.bold: true
          font.letterSpacing: 1.0
          MouseArea {
            id: lookMouse
            anchors.fill: parent
            anchors.margins: -Style.space(5)
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: {
              var draft = input.text
              input.text = ""
              root.lookRequested(draft)
            }
          }
        }

        Text {
          anchors.right: parent.right
          anchors.verticalCenter: parent.verticalCenter
          visible: root.busy
          textFormat: Text.PlainText
          text: root.brokerActing ? "acting · ctrl+c stops" : "ctrl+c stops"
          color: root.textTertiary
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
        }
      }

      Item {
        id: footer
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        height: root.footerHeight

        Text {
          id: footerHintText
          anchors.left: parent.left
          anchors.verticalCenter: parent.verticalCenter
          textFormat: Text.PlainText
          text: root.footerHint
          width: Math.min(implicitWidth, parent.width * 0.72)
          elide: Text.ElideRight
          color: root.textTertiary
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          font.bold: true
          font.letterSpacing: 1.0
        }

        TextMetrics {
          id: hintMetrics
          font: footerHintText.font
          text: root.footerHint
        }

        Text {
          id: modelText
          anchors.right: parent.right
          anchors.verticalCenter: parent.verticalCenter
          width: Math.min(implicitWidth, parent.width * 0.5)
          horizontalAlignment: Text.AlignRight
          elide: Text.ElideLeft
          textFormat: Text.PlainText
          text: root.modelName
          // The model is a whisper, not a status: give way to the key hints
          // rather than colliding with them.
          visible: !root.setupMode && text.length > 0
            && hintMetrics.advanceWidth + implicitWidth + Style.space(16) < parent.width
          color: root.textTertiary
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          font.letterSpacing: 0.6
        }
      }

      // Lazy: a mistake in the settings page cannot take the chat down with it.
      Loader {
        id: settingsLoader
        anchors.fill: parent
        active: root.settingsOpen
        visible: active
        source: "SettingsPanel.qml"
        onLoaded: {
          item.state = Qt.binding(function() { return root.settingsState })
          item.webProvider = Qt.binding(function() { return root.webProvider })
          item.webKeySet = Qt.binding(function() { return root.webKeySet })
          item.trusted = Qt.binding(function() { return root.trusted })
          item.localReachable = Qt.binding(function() { return root.localReachable })
          item.localEndpoint = Qt.binding(function() { return root.localEndpoint })
          item.locked = Qt.binding(function() { return root.busy })
          item.forceActiveFocus()
        }
        onStatusChanged: {
          if (status === Loader.Error && root.settingsOpen) root.settingsClosed()
        }
      }

      Connections {
        target: settingsLoader.item
        enabled: settingsLoader.status === Loader.Ready
        function onClose() { root.settingsClosed() }
        function onChooseModel(role, provider, id) {
          root.settingsModelChosen(role, provider, id)
        }
        function onSetAuto(on) { root.settingsAutoChanged(on) }
        function onSetWebProvider(provider) { root.settingsWebProviderChosen(provider) }
        function onSaveWebKey(key) { root.settingsWebKeySaved(key) }
        function onSetTrust(on) { root.settingsTrustChanged(on) }
        function onSetAutoPicks(on) { root.settingsAutoPicksChanged(on) }
        function onSetLocalOnly(on) { root.settingsLocalOnlyChanged(on) }
        function onSetDesktopWidget(on) { root.settingsDesktopChanged(on) }
        function onWelcomeRequested() { root.welcomeRequested() }
        function onOpenCosts() { root.costsRequested() }
        function onDiscover() { root.settingsDiscover() }
        function onRefresh() { root.settingsRefresh() }
      }

      Loader {
        id: welcomeLoader
        anchors.fill: parent
        active: root.welcomeOpen
        visible: active
        source: "Welcome.qml"
        onLoaded: {
          item.state = Qt.binding(function() { return root.welcomeState })
          item.forceActiveFocus()
        }
        onStatusChanged: {
          if (status === Loader.Error && root.welcomeOpen) root.welcomeClosed()
        }
      }

      Connections {
        target: welcomeLoader.item
        enabled: welcomeLoader.status === Loader.Ready
        function onClose() { root.welcomeClosed() }
        function onAsk(prompt) { root.welcomeAsked(prompt) }
        function onSettings() { root.settingsRequested() }
      }

      Loader {
        id: permissionsLoader
        anchors.fill: parent
        active: root.permissionsOpen
        visible: active
        source: "PermissionsPanel.qml"
        onLoaded: {
          item.state = Qt.binding(function() { return root.permissionsState })
          item.locked = Qt.binding(function() { return root.busy })
          item.forceActiveFocus()
        }
        onStatusChanged: {
          if (status === Loader.Error && root.permissionsOpen) root.permissionsClosed()
        }
      }

      Connections {
        target: permissionsLoader.item
        enabled: permissionsLoader.status === Loader.Ready
        function onClose() { root.permissionsClosed() }
        function onGrant(capability, seconds) { root.permissionsGrant(capability, seconds) }
        function onGrantAll(seconds) { root.permissionsGrantAll(seconds) }
        function onRevoke(capability) { root.permissionsRevoke(capability) }
        function onRevokeAll() { root.permissionsRevokeAll() }
        function onRefresh() { root.permissionsRefresh() }
      }

      Loader {
        id: costsLoader
        anchors.fill: parent
        active: root.costsOpen
        visible: active
        source: "CostsPanel.qml"
        onLoaded: {
          item.state = Qt.binding(function() { return root.costsState })
          item.forceActiveFocus()
        }
        onStatusChanged: {
          if (status === Loader.Error && root.costsOpen) root.costsClosed()
        }
      }

      Connections {
        target: costsLoader.item
        enabled: costsLoader.status === Loader.Ready
        function onClose() { root.costsClosed() }
        function onRefresh() { root.costsRefresh() }
      }
    }
  }

  readonly property bool statusHollow: !root.busy && root.runtimeFault.length === 0
    && root.runtimeLive && !root.grantsOn

  readonly property string footerHint: {
    if (root.setupMode) return root.setupMissing
      ? "R RETRY  ·  C COPY  ·  O PI.DEV  ·  S SETTINGS"
      : "R RETRY  ·  S SETTINGS  ·  ESC"
    if (root.historyMode) return "↑↓ CHOOSE  ·  ENTER RESUME  ·  ESC BACK"
    if (root.busy) return "WORKING  ·  STOP  ·  SUPER+ALT+ESC  ·  ESC"
    var base = "ENTER  ·  ESC"
    // The card is 547px wide: three spelled-out shortcuts plus the model name do
    // not fit, and the hint elided to "…" (LOOK lives on the composer anyway).
    if (root.transcript && root.transcript.count > 0)
      base = "CTRL+K NEW  ·  CTRL+H HISTORY  ·  ESC"
    return base
  }

  // "provider/model" is a vendor path, not a label: show the model alone.
  readonly property string modelName: {
    var id = String(root.modelLabel || "")
    var cut = id.lastIndexOf("/")
    return cut >= 0 ? id.slice(cut + 1) : id
  }

  readonly property color statusColor: {
    if (root.setupMode) return root.accent
    if (root.runtimeFault.length > 0) return root.urgent
    if (root.actionMode || root.brokerConfirm || root.brokerActing) return root.accent
    if (root.consentMode || root.looking || root.lookState === "attached") return root.accent
    if (root.recording || root.transcribing) return root.accent
    if (root.busy) return root.accent
    if (root.runtimeLive) return root.grantsOn ? root.accent : root.textSecondary
    if (root.runtimeStarting) return root.textTertiary
    return root.urgent
  }

  readonly property string statusLabel: {
    if (root.setupMode) return "SETUP"
    if (root.runtimeFault.length > 0) return "OFFLINE"
    if (root.brokerConfirm) return "CONFIRM ?"
    if (root.actionConfirm) return "ACT ?"
    if (root.actionRunning || root.brokerActing) return "ACTING"
    if (root.consentMode) return "SCREEN ?"
    if (root.looking) return "LOOKING"
    if (root.recording) return "LISTENING"
    if (root.transcribing) return "TRANSCRIBING"
    if (root.lookState === "attached") return "SCREEN"
    if (root.busy) {
      var label = root.waiting ? "WAITING" : "THINKING"
      if (root.busySeconds >= 5) label += " " + root.busySeconds + "S"
      return label
    }
    if (root.runtimeLive) {
      // Trusted is a stronger statement than a grant with minutes left.
      if (root.trusted) return "TRUSTED ›"
      if (root.grantsOn) return (root.grantLabel.length ? root.grantLabel : "ALLOWED") + " ›"
      return "ASK FIRST ›"
    }
    if (root.runtimeStarting) return "CONNECTING"
    return "OFFLINE"
  }

  property int busySeconds: 0

  Timer {
    id: nudgeTimer
    interval: 420
    repeat: false
    onTriggered: root.nudge = false
  }

  // Counts up while a run is in flight, so a slow model reads as slow rather
  // than as a frozen overlay.
  Timer {
    interval: 1000
    repeat: true
    running: root.busy
    onTriggered: root.busySeconds += 1
    onRunningChanged: if (!running) root.busySeconds = 0
  }
}
