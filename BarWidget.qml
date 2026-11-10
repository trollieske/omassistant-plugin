import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Commons as Commons
import qs.Ui

// The bar half of Omassistant: one diamond, a status dot, and a click that
// summons the overlay. It holds no conversation of its own — the overlay does —
// so the only state it reads is the small runtime snapshot the overlay writes
// to $XDG_RUNTIME_DIR on transitions.
BarWidget {
  id: root
  moduleName: "omassistant"

  readonly property string runtimeDir: Quickshell.env("XDG_RUNTIME_DIR") || ""
  readonly property string statePath: runtimeDir.length ? runtimeDir + "/omassistant/state.json" : ""

  property bool live: false
  property bool busy: false
  property bool waiting: false
  property bool unread: false
  property string fault: ""
  property string dictation: "idle"

  readonly property bool listening: dictation === "recording" || dictation === "transcribing"
  readonly property bool attention: busy || unread || listening || fault.length > 0
  readonly property color dotColor: fault.length > 0 ? Commons.Color.urgent
    : (busy || unread || listening ? Commons.Color.accent : "transparent")
  readonly property string tooltip: fault.length > 0
    ? "Omassistant — offline (SUPER+ALT+A)"
    : (busy ? "Omassistant — thinking (SUPER+ALT+A)" : "Omassistant — SUPER+ALT+A")

  function applyState(raw) {
    var data = null
    try {
      data = JSON.parse(String(raw || ""))
    } catch (error) {
      data = null
    }
    if (!data || typeof data !== "object") {
      live = false
      busy = false
      waiting = false
      unread = false
      fault = ""
      return
    }
    live = data.live === true
    busy = data.busy === true
    waiting = data.waiting === true
    unread = data.unread === true
    fault = typeof data.fault === "string" ? data.fault : ""
    dictation = typeof data.dictation === "string" ? data.dictation : "idle"
  }

  function toggleOverlay() {
    Quickshell.execDetached(["omarchy-shell", "shell", "toggle", "omassistant", "{}"])
  }

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  WidgetButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: "◆"
    labelVisible: true
    hasVisualContent: true
    tooltipText: root.tooltip
    // The diamond is a mark, not a label: a hotkey press shouldn't shrink it.
    pressFeedbackButtons: Qt.NoButton

    onPressed: function(button) {
      if (button === Qt.LeftButton) root.toggleOverlay()
      else if (button === Qt.MiddleButton) Quickshell.execDetached(["omarchy-shell", "omassistant", "clear"])
    }

    // A quiet dot: accent while working or with an unread answer, urgent when
    // the runtime is offline. Invisible otherwise, so the bar stays calm.
    Rectangle {
      id: dot
      visible: root.attention
      width: Math.max(4, Style.space(6))
      height: width
      radius: width / 2
      color: root.dotColor
      // A badge on the diamond's lower-right corner, inside the bar surface:
      // a dot above the slot would be clipped by the top edge of the screen.
      anchors.right: parent.right
      anchors.rightMargin: -Math.round(width * 0.2)
      anchors.bottom: parent.bottom
      anchors.bottomMargin: Math.max(3, Style.space(5))

      SequentialAnimation on opacity {
        running: (root.busy || root.listening) && !Style.reduceMotion
        loops: Animation.Infinite
        NumberAnimation { from: 1; to: 0.2; duration: 640; easing.type: Easing.InOutSine }
        NumberAnimation { from: 0.2; to: 1; duration: 640; easing.type: Easing.InOutSine }
      }
      onVisibleChanged: if (!visible) opacity = 1
    }
  }

  ShellIpc {
    target: "omassistant.bar"
    function state(): string {
      return JSON.stringify({
        path: root.statePath,
        live: root.live,
        busy: root.busy,
        unread: root.unread,
        fault: root.fault,
        dictation: root.dictation,
        attention: root.attention
      })
    }
  }

  FileView {
    id: stateFile
    path: root.statePath
    watchChanges: true
    printErrors: false
    onLoaded: root.applyState(text())
    onLoadFailed: root.applyState("")
    onFileChanged: reload()
  }
}
