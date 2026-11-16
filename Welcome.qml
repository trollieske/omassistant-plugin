import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui

// The first-run card: what is set up, what it will ask for, and what to try.
//
// It is a sheet in the same idiom as permissions and costs — one surface, one
// header, key hints at the bottom — because the first thing a person sees should
// be the same thing every other screen is, not a separate wizard.
//
// The state it reads is the assistant's own: whether pi is there, which model the
// routing picked, whether the broker is up, and how many capabilities are granted.
// Nothing here is claimed unless it is true, which is why "not yet" is a normal
// line rather than an error.
FocusScope {
  id: root
  objectName: "omassistantWelcome"

  property var state: ({})
  property int selected: 0

  signal close()
  signal ask(string prompt)
  signal settings()

  readonly property var st: root.state && typeof root.state === "object" ? root.state : ({})
  readonly property bool piReady: root.st.piReady === true
  readonly property bool broker: root.st.broker === true
  readonly property string model: String(root.st.model || "")
  readonly property int granted: Number(root.st.granted || 0)
  readonly property string version: String(root.st.version || "")
  readonly property var ideas: root.st.ideas && root.st.ideas.length ? root.st.ideas : []

  readonly property color surface: Commons.Color.menu.background
  readonly property color foreground: Commons.Color.menu.text
  readonly property color accent: Commons.Color.accent
  readonly property color urgent: Commons.Color.urgent
  readonly property color textSecondary: Commons.Util.alpha(root.foreground, 0.78)
  readonly property color textTertiary: Commons.Util.alpha(root.foreground, 0.6)
  readonly property color separator: Commons.Util.alpha(root.foreground, 0.12)
  readonly property color selectedFill: Commons.Color.menu.selectedBackground
  readonly property string fontFamily: Style.font.menuFamily
  readonly property int rowHeight: Style.space(46)

  function move(delta) {
    if (!root.ideas.length) return
    root.selected = (root.selected + delta + root.ideas.length) % root.ideas.length
  }

  function choose() {
    if (root.selected < 0 || root.selected >= root.ideas.length) return
    root.ask(String(root.ideas[root.selected].prompt || ""))
  }

  Component.onCompleted: {
    root.selected = 0
    root.forceActiveFocus()
  }

  Keys.onPressed: function(event) {
    if (event.key === Qt.Key_Escape) {
      root.close()
      event.accepted = true
    } else if (event.key === Qt.Key_Up) {
      root.move(-1)
      event.accepted = true
    } else if (event.key === Qt.Key_Down) {
      root.move(1)
      event.accepted = true
    } else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
      root.choose()
      event.accepted = true
    } else if (event.key === Qt.Key_S) {
      root.settings()
      event.accepted = true
    }
  }

  Rectangle {
    anchors.fill: parent
    color: root.surface
  }

  MouseArea {
    anchors.fill: parent
    onClicked: root.forceActiveFocus()
  }

  Item {
    id: header
    anchors.top: parent.top
    anchors.left: parent.left
    anchors.right: parent.right
    height: Style.space(28)

    Text {
      anchors.left: parent.left
      anchors.top: parent.top
      textFormat: Text.PlainText
      text: "WELCOME"
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
      font.bold: true
      font.letterSpacing: 1.6
    }

    Text {
      anchors.right: parent.right
      anchors.top: parent.top
      textFormat: Text.PlainText
      text: "ESC CLOSE"
      color: closeMouse.containsMouse ? root.accent : root.textTertiary
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      font.bold: true
      font.letterSpacing: 1.0
      MouseArea {
        id: closeMouse
        anchors.fill: parent
        anchors.margins: -Style.space(4)
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: root.close()
      }
    }

    Rectangle {
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.bottom: parent.bottom
      height: 1
      color: root.separator
    }
  }

  Column {
    id: body
    anchors.top: header.bottom
    anchors.topMargin: Style.space(22)
    anchors.left: parent.left
    anchors.right: parent.right
    spacing: Style.space(20)

    Text {
      width: parent.width
      textFormat: Text.PlainText
      text: "Your desktop, with an assistant that asks first."
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.heading
      font.bold: true
      wrapMode: Text.Wrap
    }

    Text {
      width: parent.width
      textFormat: Text.PlainText
      text: "I live in the bar. I can look at a window, open an app, run one command in your "
        + "shell, and act in a window — every one of those is a separate permission, and "
        + "nothing happens until you say yes."
      color: root.textSecondary
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
      wrapMode: Text.Wrap
      lineHeight: 1.25
    }

    Column {
      width: parent.width
      spacing: Style.space(10)

      Text {
        textFormat: Text.PlainText
        text: "SETUP"
        color: root.textTertiary
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        font.bold: true
        font.letterSpacing: 1.2
      }

      Repeater {
        model: [
          { label: "pi runtime",
            detail: root.piReady ? "running over RPC" : "run `pi --version` once; Omarchy installs it with mise",
            ok: root.piReady },
          { label: "Model",
            detail: root.model.length ? root.model : "none chosen yet — open settings",
            ok: root.model.length > 0 },
          { label: "Broker",
            detail: root.broker ? "omassistantd is connected" : "not running yet; the card starts it on demand",
            ok: root.broker },
          { label: "Permissions",
            detail: root.granted > 0
              ? root.granted + (root.granted === 1 ? " grant is live" : " grants are live")
              : "nothing granted — I will ask each time",
            ok: root.granted > 0 }
        ]

        delegate: Item {
          required property var modelData
          width: body.width
          height: Math.max(Style.space(30), label.implicitHeight + detail.implicitHeight + Style.space(4))

          Text {
            id: mark
            anchors.left: parent.left
            anchors.top: parent.top
            anchors.topMargin: Style.space(2)
            textFormat: Text.PlainText
            text: modelData.ok ? "✓" : "·"
            color: modelData.ok ? root.accent : root.textTertiary
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            font.bold: true
          }

          Text {
            id: label
            anchors.left: mark.right
            anchors.leftMargin: Style.space(10)
            anchors.right: parent.right
            textFormat: Text.PlainText
            text: modelData.label
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
            font.bold: true
            elide: Text.ElideRight
          }

          Text {
            id: detail
            anchors.left: label.left
            anchors.right: parent.right
            anchors.top: label.bottom
            anchors.topMargin: Style.space(1)
            textFormat: Text.PlainText
            text: modelData.detail
            color: modelData.ok ? root.textTertiary : root.textSecondary
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            elide: Text.ElideRight
          }
        }
      }
    }

    Column {
      width: parent.width
      spacing: Style.space(8)

      Text {
        textFormat: Text.PlainText
        text: "TRY ASKING"
        color: root.textTertiary
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        font.bold: true
        font.letterSpacing: 1.2
      }

      ListView {
        id: ideaList
        width: parent.width
        height: Math.min(root.ideas.length, 4) * root.rowHeight
        interactive: false
        clip: true
        model: root.ideas
        spacing: 0

        delegate: Item {
          required property int index
          required property var modelData
          width: ideaList.width
          height: root.rowHeight

          readonly property bool isSelected: index === root.selected

          Rectangle {
            anchors.fill: parent
            color: parent.isSelected ? root.selectedFill : "transparent"
          }
          Rectangle {
            anchors.left: parent.left
            anchors.top: parent.top
            anchors.bottom: parent.bottom
            width: Style.space(2)
            color: parent.isSelected ? root.accent : "transparent"
          }

          Text {
            anchors.left: parent.left
            anchors.leftMargin: Style.space(10)
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            textFormat: Text.PlainText
            text: String(parent.modelData.prompt || "")
            color: parent.isSelected ? root.accent : root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
            elide: Text.ElideRight
          }

          MouseArea {
            anchors.fill: parent
            cursorShape: Qt.PointingHandCursor
            onClicked: {
              root.selected = parent.index
              root.forceActiveFocus()
              root.choose()
            }
          }
        }
      }
    }
  }

  Item {
    id: footer
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.bottom: parent.bottom
    height: Style.space(20)

    Text {
      anchors.left: parent.left
      anchors.verticalCenter: parent.verticalCenter
      textFormat: Text.PlainText
      text: "↑↓ CHOOSE  ·  ENTER ASK  ·  S SETTINGS  ·  ESC CLOSE"
      color: root.textTertiary
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      font.bold: true
      font.letterSpacing: 0.8
    }
  }
}
