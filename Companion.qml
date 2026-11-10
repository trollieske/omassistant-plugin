import QtQuick
import Quickshell
import Quickshell.Hyprland
import Quickshell.Wayland
import qs.Commons
import qs.Commons as Commons
import qs.Ui

// The desktop companion: a small, always-there card that says what Omassistant
// is doing and opens the overlay on click. It carries no keyboard state of its
// own — the hotkey and the overlay are the keyboard path — so it is a glance,
// not a control panel.
//
// It minimizes to a single diamond (the chip keeps the blink and the mood
// colour), and the state survives a shell restart through the host's ui.json.
PanelWindow {
  id: window

  property string microcopy: "no thoughts yet"
  property string peek: ""
  property string dictation: "idle"
  property bool collapsed: false
  property bool busy: false
  property bool waiting: false
  property bool offline: false
  property string hotkey: "SUP+ALT+A"

  signal activated()
  signal dismissed()
  signal collapseToggled()

  readonly property color foreground: Commons.Color.menu.text
  readonly property color accent: Commons.Color.accent
  readonly property color urgent: Commons.Color.urgent
  readonly property color textSecondary: Commons.Util.alpha(foreground, 0.78)
  readonly property color textTertiary: Commons.Util.alpha(foreground, 0.62)
  readonly property string fontFamily: Style.font.menuFamily
  readonly property bool highlighted: mouse.containsMouse || mouse.pressed
  readonly property bool listening: dictation === "recording" || dictation === "transcribing"
  readonly property int cornerRadius: Style.cornerRadius
  readonly property int padding: Style.spacing.controlPaddingX
  readonly property int gap: Style.space(3)
  readonly property int expandedWidth: Style.space(330)
  readonly property int collapsedWidth: Style.space(58)
  readonly property int collapsedHeight: Style.space(42)

  // A stopped animation keeps the last frame it wrote, so both the blink and
  // the breathing pulse are reset when the runtime state flips.
  onBusyChanged: {
    if (busy) eye.opacity = 1
    else eye.scale = 1
  }

  // Follows the monitor in use instead of putting a twin on every output.
  readonly property var targetScreen: {
    var name = Hyprland.focusedMonitor ? String(Hyprland.focusedMonitor.name || "") : ""
    for (var i = 0; i < Quickshell.screens.length; i++) {
      if (Quickshell.screens[i].name === name) return Quickshell.screens[i]
    }
    return Quickshell.screens.length > 0 ? Quickshell.screens[0] : null
  }

  screen: targetScreen
  color: "transparent"
  exclusionMode: ExclusionMode.Ignore
  WlrLayershell.layer: WlrLayer.Overlay
  WlrLayershell.namespace: "omassistant-companion"
  WlrLayershell.keyboardFocus: WlrKeyboardFocus.None

  anchors {
    bottom: true
    right: true
  }
  margins {
    bottom: Style.space(104)
    right: Style.space(22)
  }
  implicitWidth: card.width
  implicitHeight: card.height

  BorderSurface {
    id: card
    width: window.collapsed ? window.collapsedWidth : window.expandedWidth
    height: window.collapsed ? window.collapsedHeight
      : card.contentTopInset + row1.height
        + (peekText.visible ? window.gap + peekText.implicitHeight : 0)
        + card.contentBottomInset
    // Opaque like the shell's panels: a translucent card over a busy window
    // makes the peek unreadable, which defeats the point of the widget.
    color: Commons.Color.menu.background
    borderSpec: Border.surfaceSpec("menu", "border",
      window.highlighted ? window.accent : Commons.Color.menu.border,
      Math.max(1, Style.space(2)))
    radius: window.cornerRadius
    padding: window.collapsed ? 0 : window.padding

    Behavior on width { NumberAnimation { duration: Style.duration(180); easing.type: Easing.Bezier; easing.bezierCurve: Style.easeOut } }
    Behavior on height { NumberAnimation { duration: Style.duration(180); easing.type: Easing.Bezier; easing.bezierCurve: Style.easeOut } }

    // The eye blinks on its own schedule when idle and breathes while an
    // answer is being written — the whole personality of the widget, and the
    // only thing left when it is minimized.
    Text {
      id: eye
      textFormat: Text.PlainText
      text: "◆"
      color: window.offline ? window.urgent : window.accent
      font.family: window.fontFamily
      font.pixelSize: window.collapsed ? Style.font.heading : Style.font.title
      x: window.collapsed
        ? Math.round((card.width - width) / 2) - Math.round(Style.space(6))
        : card.contentLeftInset
      y: window.collapsed
        ? Math.round((card.height - height) / 2)
        : card.contentTopInset + Math.round((row1.height - height) / 2)

      SequentialAnimation {
        running: !window.busy && !window.listening && !Style.reduceMotion
        loops: Animation.Infinite
        PauseAnimation {
          duration: 2600 + Math.round(Math.random() * 3600)
        }
        NumberAnimation {
          target: eye
          property: "opacity"
          from: 1; to: 0.15
          duration: 70
        }
        NumberAnimation {
          target: eye
          property: "opacity"
          from: 0.15; to: 1
          duration: 120
        }
      }

      SequentialAnimation on scale {
        running: (window.busy || window.listening) && !Style.reduceMotion
        loops: Animation.Infinite
        NumberAnimation { from: 1; to: 1.18; duration: 620; easing.type: Easing.InOutSine }
        NumberAnimation { from: 1.18; to: 1; duration: 620; easing.type: Easing.InOutSine }
      }
    }

    Item {
      id: row1
      visible: !window.collapsed
      x: card.contentLeftInset + eye.width + Style.space(9)
      y: card.contentTopInset
      width: card.width - card.contentLeftInset - card.contentRightInset - eye.width - Style.space(9)
      height: Math.max(eye.implicitHeight, hotkeyChip.height)

      // The hotkey chip is a glanceable reminder, not a control.
      BorderSurface {
        id: hotkeyChip
        anchors.right: parent.right
        anchors.rightMargin: Style.space(16)
        anchors.verticalCenter: parent.verticalCenter
        implicitWidth: hotkeyText.implicitWidth + Style.space(8)
        implicitHeight: hotkeyText.implicitHeight + Style.space(3)
        color: "transparent"
        borderSpec: Border.controlSpec("normal", window.foreground, window.accent)
        radius: window.cornerRadius

        Text {
          id: hotkeyText
          anchors.centerIn: parent
          textFormat: Text.PlainText
          text: window.hotkey
          color: window.textTertiary
          font.family: window.fontFamily
          font.pixelSize: Style.font.caption
          font.letterSpacing: 0.8
        }
      }

      Text {
        id: copy
        anchors.left: parent.left
        anchors.right: hotkeyChip.left
        anchors.rightMargin: Style.space(8)
        anchors.verticalCenter: parent.verticalCenter
        textFormat: Text.PlainText
        text: window.microcopy
        color: window.offline ? window.urgent : (window.busy || window.listening ? window.accent : window.foreground)
        font.family: window.fontFamily
        font.pixelSize: Style.font.body
        font.bold: window.busy || window.offline || window.listening
        elide: Text.ElideRight
      }
    }

    // Row 2 is the interesting bit when there is one: the last answer while
    // idle, the question being worked on while busy.
    Text {
      id: peekText
      visible: !window.collapsed
      x: row1.x
      y: card.contentTopInset + row1.height + window.gap
      width: card.width - x - card.contentRightInset
      textFormat: Text.PlainText
      text: window.peek
      elide: Text.ElideRight
      color: window.textSecondary
      font.family: window.fontFamily
      font.pixelSize: Style.font.bodySmall
      opacity: 0.92
    }

    // The minimize handle: a quiet chevron at the card's right edge, a little
    // louder on hover. Consistent in both states so the way back is obvious.
    Item {
      id: collapseHandle
      width: Math.max(18, Style.space(22))
      height: Math.max(18, Style.space(22))
      anchors.right: parent.right
      anchors.rightMargin: window.collapsed ? Math.round(Style.space(4)) : Math.round(Style.space(2))
      anchors.verticalCenter: parent.verticalCenter
      z: 3

      Text {
        anchors.centerIn: parent
        textFormat: Text.PlainText
        text: window.collapsed ? "›" : "⌄"
        color: handleMouse.containsMouse ? window.accent : window.textTertiary
        font.family: window.fontFamily
        font.pixelSize: Style.font.title
      }

      MouseArea {
        id: handleMouse
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: window.collapseToggled()
      }
    }

    MouseArea {
      id: mouse
      anchors.fill: parent
      acceptedButtons: Qt.LeftButton | Qt.RightButton
      hoverEnabled: true
      cursorShape: Qt.PointingHandCursor
      onClicked: function(event) {
        if (event.button === Qt.RightButton) window.dismissed()
        else window.activated()
      }
    }
  }
}
