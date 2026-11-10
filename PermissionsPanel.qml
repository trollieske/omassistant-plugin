import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui

// The permissions sheet. Six capabilities, one duration, one obvious "allow
// everything" button. It is lazy-loaded by ChatPanel and only talks to the host
// through signals; the broker does the granting and the audit.
FocusScope {
  id: root
  objectName: "omassistantPermissions"

  property var state: ({})
  property bool locked: false
  property int selectedSeconds: 900
  property int selected: 0

  signal close()
  signal grant(string capability, int seconds)
  signal grantAll(int seconds)
  signal revoke(string capability)
  signal revokeAll()
  signal refresh()

  readonly property var st: root.state && typeof root.state === "object" ? root.state : ({})
  readonly property var capabilities: root.st.capabilities ? root.st.capabilities : []
  readonly property var durations: root.st.durations ? root.st.durations : [300, 600, 900, 1200, 1500, 1800, 3600]
  readonly property bool broker: root.st.broker === true
  readonly property bool busy: root.st.busy === true
  readonly property bool lookPending: root.st.lookPending === true
  readonly property int grantCount: Number(root.st.grantCount || 0)

  readonly property color surface: Commons.Color.menu.background
  readonly property color foreground: Commons.Color.menu.text
  readonly property color accent: Commons.Color.accent
  readonly property color urgent: Commons.Color.urgent
  readonly property color textSecondary: Commons.Util.alpha(root.foreground, 0.78)
  readonly property color textTertiary: Commons.Util.alpha(root.foreground, 0.6)
  readonly property color separator: Commons.Util.alpha(root.foreground, 0.12)
  readonly property color selectedFill: Commons.Color.menu.selectedBackground
  readonly property string fontFamily: Style.font.menuFamily

  function durationLabel(seconds) {
    var value = Number(seconds)
    if (!isFinite(value)) return ""
    if (value >= 3600) return Math.round(value / 3600) + " HOUR" + (value >= 7200 ? "S" : "")
    return Math.round(value / 60) + " MIN"
  }

  function durationShort(seconds) {
    var value = Number(seconds)
    if (value >= 3600) return Math.round(value / 3600) + "H"
    return Math.round(value / 60) + "M"
  }

  function selectDuration(delta) {
    if (!root.durations.length) return
    var index = root.durations.indexOf(root.selectedSeconds)
    if (index < 0) index = 0
    index = (index + delta + root.durations.length) % root.durations.length
    root.selectedSeconds = Number(root.durations[index])
  }

  function selectRow(delta) {
    if (!root.capabilities.length) return
    root.selected = (root.selected + delta + root.capabilities.length) % root.capabilities.length
    rowList.positionViewAtIndex(root.selected, ListView.Contain)
  }

  function rowAt(index) {
    return index >= 0 && index < root.capabilities.length ? root.capabilities[index] : null
  }

  function activate() {
    var row = root.rowAt(root.selected)
    if (!row) return
    if (row.granted) root.revoke(row.name)
    else root.grant(row.name, root.selectedSeconds)
  }

  Keys.onPressed: function(event) {
    if (event.key === Qt.Key_Escape) {
      root.close()
      event.accepted = true
      return
    }
    if (event.key === Qt.Key_Up) { root.selectRow(-1); event.accepted = true }
    else if (event.key === Qt.Key_Down) { root.selectRow(1); event.accepted = true }
    else if (event.key === Qt.Key_Left) { root.selectDuration(-1); event.accepted = true }
    else if (event.key === Qt.Key_Right) { root.selectDuration(1); event.accepted = true }
    else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
      root.activate()
      event.accepted = true
    } else if (event.key === Qt.Key_A) {
      root.grantAll(root.selectedSeconds)
      event.accepted = true
    } else if (event.key === Qt.Key_R) {
      if (root.grantCount > 0) root.revokeAll()
      event.accepted = true
    }
  }

  Component.onCompleted: root.forceActiveFocus()

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
      text: "PERMISSIONS"
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
      color: root.textTertiary
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      font.bold: true
      font.letterSpacing: 1.0
      MouseArea {
        anchors.fill: parent
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

  Item {
    id: intro
    anchors.top: header.bottom
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.topMargin: Style.space(10)
    height: Style.space(34)

    Text {
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.top: parent.top
      textFormat: Text.PlainText
      text: root.broker
        ? "Allow the assistant for a while. Every call is still audited; revoke here or with SUPER+SHIFT+ESC."
        : "The broker is not connected. Open the assistant once, then try again."
      color: root.broker ? root.textSecondary : root.urgent
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      wrapMode: Text.Wrap
      maximumLineCount: 2
      elide: Text.ElideRight
    }
  }

  Item {
    id: durationRow
    anchors.top: intro.bottom
    anchors.left: parent.left
    anchors.right: parent.right
    height: Style.space(36)

    Text {
      anchors.left: parent.left
      anchors.verticalCenter: parent.verticalCenter
      textFormat: Text.PlainText
      text: "ALLOW FOR"
      color: root.textTertiary
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      font.bold: true
      font.letterSpacing: 1.0
    }

    Row {
      anchors.left: parent.left
      anchors.leftMargin: Style.space(64)
      anchors.verticalCenter: parent.verticalCenter
      spacing: Style.space(5)

      Repeater {
        model: root.durations
        delegate: Item {
          property int seconds: Number(modelData)
          width: Math.max(Style.space(22), durationText.implicitWidth + Style.space(6))
          height: Style.space(22)

          Rectangle {
            anchors.fill: parent
            color: root.selectedSeconds === seconds ? root.accent : "transparent"
            border.width: 1
            border.color: root.selectedSeconds === seconds ? root.accent : root.separator
          }

          Text {
            id: durationText
            anchors.centerIn: parent
            textFormat: Text.PlainText
            text: root.durationShort(seconds)
            color: root.selectedSeconds === seconds ? root.surface : root.textSecondary
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            font.bold: true
          }

          MouseArea {
            anchors.fill: parent
            cursorShape: Qt.PointingHandCursor
            onClicked: {
              root.selectedSeconds = seconds
              root.forceActiveFocus()
            }
          }
        }
      }
    }
  }

  Item {
    id: allowAllRow
    anchors.top: durationRow.bottom
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.topMargin: Style.space(8)
    height: Style.space(46)

    Rectangle {
      id: allowAllButton
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.top: parent.top
      height: Style.space(40)
      color: allowAllMouse.containsMouse ? root.selectedFill : "transparent"
      border.width: 1
      border.color: root.accent

      Text {
        anchors.centerIn: parent
        textFormat: Text.PlainText
        text: "ALLOW EVERYTHING FOR " + root.durationLabel(root.selectedSeconds)
        color: root.accent
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
        font.bold: true
        font.letterSpacing: 1.0
      }

      MouseArea {
        id: allowAllMouse
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: root.grantAll(root.selectedSeconds)
      }
    }
  }

  Text {
    id: warning
    anchors.top: allowAllRow.bottom
    anchors.left: parent.left
    anchors.right: parent.right
    textFormat: Text.PlainText
    text: root.lookPending
      ? "This will also take the look you started. Until the timer ends, the assistant can act without asking."
      : "Until the timer ends, the assistant can act without asking. Every call is still audited."
    color: root.lookPending ? root.accent : root.textTertiary
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    wrapMode: Text.Wrap
    maximumLineCount: 2
    elide: Text.ElideRight
  }

  Item {
    id: listHeader
    anchors.top: warning.bottom
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.topMargin: Style.space(12)
    height: Style.space(22)

    Text {
      anchors.left: parent.left
      anchors.verticalCenter: parent.verticalCenter
      textFormat: Text.PlainText
      text: "OR ONE AT A TIME"
      color: root.textTertiary
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      font.bold: true
      font.letterSpacing: 1.0
    }

    Text {
      anchors.right: parent.right
      anchors.verticalCenter: parent.verticalCenter
      visible: root.grantCount > 0
      textFormat: Text.PlainText
      text: "REVOKE ALL"
      color: revokeAllMouse.containsMouse ? root.urgent : root.textTertiary
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      font.bold: true
      font.letterSpacing: 1.0
      MouseArea {
        id: revokeAllMouse
        anchors.fill: parent
        anchors.margins: -Style.space(4)
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: root.revokeAll()
      }
    }
  }

  ListView {
    id: rowList
    anchors.top: listHeader.bottom
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.bottom: footer.top
    anchors.bottomMargin: Style.space(6)
    clip: true
    model: root.capabilities
    currentIndex: root.selected
    boundsBehavior: Flickable.StopAtBounds
    spacing: 0

    delegate: Item {
      property var row: modelData
      width: rowList.width
      height: Style.space(58)

      Rectangle {
        anchors.fill: parent
        visible: index === root.selected
        color: root.selectedFill
      }
      Rectangle {
        anchors.bottom: parent.bottom
        anchors.left: parent.left
        anchors.right: parent.right
        height: 1
        color: root.separator
      }

      Text {
        anchors.left: parent.left
        anchors.right: rowAction.left
        anchors.rightMargin: Style.space(12)
        anchors.top: parent.top
        anchors.topMargin: Style.space(7)
        textFormat: Text.PlainText
        text: row.label
        color: row.granted ? root.accent : root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
        font.bold: true
        elide: Text.ElideRight
      }

      Text {
        anchors.left: parent.left
        anchors.right: rowAction.left
        anchors.rightMargin: Style.space(12)
        anchors.top: parent.top
        anchors.topMargin: Style.space(23)
        textFormat: Text.PlainText
        text: row.detail
        color: root.textTertiary
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        elide: Text.ElideRight
        maximumLineCount: 1
      }

      Text {
        anchors.left: parent.left
        anchors.right: rowAction.left
        anchors.rightMargin: Style.space(12)
        anchors.top: parent.top
        anchors.topMargin: Style.space(38)
        textFormat: Text.PlainText
        text: row.granted ? row.labelText : "Not allowed"
        color: row.granted ? root.accent : root.textTertiary
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        font.bold: row.granted
        elide: Text.ElideRight
      }

      Rectangle {
        id: rowAction
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        width: Math.max(Style.space(54), rowActionText.implicitWidth + Style.space(14))
        height: Style.space(24)
        color: "transparent"
        border.width: 1
        border.color: row.granted ? root.separator : root.accent

        Text {
          id: rowActionText
          anchors.centerIn: parent
          textFormat: Text.PlainText
          text: row.granted ? "REVOKE" : "ALLOW " + root.durationShort(root.selectedSeconds)
          color: row.granted ? root.textSecondary : root.accent
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          font.bold: true
          font.letterSpacing: 0.8
        }

        MouseArea {
          anchors.fill: parent
          cursorShape: Qt.PointingHandCursor
          onClicked: {
            root.selected = index
            root.forceActiveFocus()
            if (row.granted) root.revoke(row.name)
            else root.grant(row.name, root.selectedSeconds)
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
      anchors.right: parent.right
      anchors.verticalCenter: parent.verticalCenter
      visible: !root.broker
      textFormat: Text.PlainText
      text: "REFRESH"
      color: refreshMouse.containsMouse ? root.accent : root.textSecondary
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      font.bold: true
      font.letterSpacing: 1.0
      MouseArea {
        id: refreshMouse
        anchors.fill: parent
        anchors.margins: -Style.space(4)
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: root.refresh()
      }
    }

    Text {
      anchors.left: parent.left
      anchors.verticalCenter: parent.verticalCenter
      textFormat: Text.PlainText
      text: "↑↓ CHOOSE · ←→ TIME · ENTER TOGGLE · A ALLOW ALL · ESC"
      color: root.textTertiary
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      font.bold: true
      font.letterSpacing: 0.8
    }
  }
}
