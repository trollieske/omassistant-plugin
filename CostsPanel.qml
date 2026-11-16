import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui

// The usage sheet: what pi reports for every assistant message, in US dollars.
// It reads only cost, tokens and model metadata; no prompt or answer text (the
// session titles come from the plugin's own index, not from pi's content).
FocusScope {
  id: root
  objectName: "omassistantCosts"

  property var state: ({})
  property int selected: 0

  signal close()
  signal refresh()

  readonly property var st: root.state && typeof root.state === "object" ? root.state : ({})
  readonly property var sessions: root.st.sessions ? root.st.sessions : []
  readonly property var models: root.st.models ? root.st.models : []
  readonly property var current: root.st.current ? root.st.current : null
  readonly property bool loading: root.st.loading === true
  readonly property string error: String(root.st.error || "")
  readonly property real total: Number(root.st.totalUsd) || 0

  readonly property color surface: Commons.Color.menu.background
  readonly property color foreground: Commons.Color.menu.text
  readonly property color accent: Commons.Color.accent
  readonly property color urgent: Commons.Color.urgent
  readonly property color textSecondary: Commons.Util.alpha(root.foreground, 0.78)
  readonly property color textTertiary: Commons.Util.alpha(root.foreground, 0.6)
  readonly property color separator: Commons.Util.alpha(root.foreground, 0.12)
  readonly property color selectedFill: Commons.Color.menu.selectedBackground
  readonly property string fontFamily: Style.font.menuFamily

  function money(value) {
    var n = Number(value)
    if (!isFinite(n) || n <= 0) return "$0.0000"
    if (n >= 1) return "$" + n.toFixed(2)
    if (n >= 0.01) return "$" + n.toFixed(4)
    return "$" + n.toFixed(6)
  }

  function move(delta) {
    if (!root.sessions.length) return
    root.selected = Math.max(0, Math.min(root.sessions.length - 1, root.selected + delta))
    sessionList.positionViewAtIndex(root.selected, ListView.Contain)
  }

  function refreshCosts() { root.refresh() }

  Keys.onPressed: function(event) {
    if (event.key === Qt.Key_Escape) { root.close(); event.accepted = true }
    else if (event.key === Qt.Key_R) { root.refreshCosts(); event.accepted = true }
    else if (event.key === Qt.Key_Up) { root.move(-1); event.accepted = true }
    else if (event.key === Qt.Key_Down) { root.move(1); event.accepted = true }
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

    // A back affordance, not only the Esc key: the sheets are the one place a
    // person can end up somewhere they did not mean to be.
    Item {
      id: backButton
      anchors.left: parent.left
      anchors.top: parent.top
      width: Style.space(22)
      height: Style.space(22)

      Rectangle {
        anchors.fill: parent
        radius: Style.space(4)
        color: backMouse.containsMouse ? Commons.Color.menu.selectedBackground : "transparent"
      }

      Text {
        anchors.centerIn: parent
        textFormat: Text.PlainText
        text: "‹"
        color: backMouse.containsMouse ? root.accent : root.textSecondary
        font.family: root.fontFamily
        font.pixelSize: Style.font.heading
      }

      MouseArea {
        id: backMouse
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: root.close()
      }
    }

    Text {
      anchors.left: backButton.right
      anchors.leftMargin: Style.space(8)
      anchors.top: parent.top
      anchors.topMargin: Style.space(3)
      textFormat: Text.PlainText
      text: "USAGE & COSTS"
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

  Text {
    id: intro
    anchors.top: header.bottom
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.topMargin: Style.space(8)
    textFormat: Text.PlainText
    text: "Provider-reported usage in US dollars. Missing usage counts as $0."
    color: root.textTertiary
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    wrapMode: Text.Wrap
    maximumLineCount: 2
    elide: Text.ElideRight
  }

  Item {
    id: totals
    anchors.top: intro.bottom
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.topMargin: Style.space(12)
    height: Style.space(84)

    Row {
      anchors.fill: parent
      spacing: Style.space(18)

      Item {
        width: (parent.width - parent.spacing) / 2
        height: parent.height
        Text {
          anchors.left: parent.left
          anchors.top: parent.top
          textFormat: Text.PlainText
          text: "THIS CONVERSATION"
          color: root.textTertiary
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          font.bold: true
          font.letterSpacing: 1.0
        }
        Text {
          anchors.left: parent.left
          anchors.top: parent.top
          anchors.topMargin: Style.space(18)
          textFormat: Text.PlainText
          text: root.money(root.current ? root.current.costUsd : 0)
          color: root.accent
          font.family: root.fontFamily
          font.pixelSize: Style.font.heading
          font.bold: true
        }
        Text {
          anchors.left: parent.left
          anchors.right: parent.right
          anchors.top: parent.top
          anchors.topMargin: Style.space(42)
          textFormat: Text.PlainText
          text: root.current ? root.current.title : (root.loading ? "reading…" : "no usage yet")
          color: root.textSecondary
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          elide: Text.ElideRight
          maximumLineCount: 1
        }
      }

      Item {
        width: (parent.width - parent.spacing) / 2
        height: parent.height
        Text {
          anchors.left: parent.left
          anchors.top: parent.top
          textFormat: Text.PlainText
          text: "ALL CONVERSATIONS"
          color: root.textTertiary
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          font.bold: true
          font.letterSpacing: 1.0
        }
        Text {
          anchors.left: parent.left
          anchors.top: parent.top
          anchors.topMargin: Style.space(18)
          textFormat: Text.PlainText
          text: root.loading ? "…" : root.money(root.total)
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.heading
          font.bold: true
        }
        Text {
          anchors.left: parent.left
          anchors.top: parent.top
          anchors.topMargin: Style.space(42)
          textFormat: Text.PlainText
          text: root.sessions.length + (root.sessions.length === 1 ? " conversation" : " conversations")
          color: root.textSecondary
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
        }
      }
    }
  }

  Text {
    id: loadingText
    anchors.top: totals.bottom
    anchors.left: parent.left
    anchors.right: parent.right
    visible: root.loading || root.error.length > 0
    textFormat: Text.PlainText
    text: root.error.length > 0 ? root.error : "Reading pi's session usage…"
    color: root.error.length > 0 ? root.urgent : root.textSecondary
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    wrapMode: Text.Wrap
    maximumLineCount: 3
    elide: Text.ElideRight
  }

  Text {
    id: modelHeading
    anchors.top: loadingText.visible ? loadingText.bottom : totals.bottom
    anchors.left: parent.left
    // Air above the heading, so a group reads as a heading over its rows rather
    // than a line pressed against the block above it.
    anchors.topMargin: Style.space(26)
    textFormat: Text.PlainText
    text: "BY MODEL"
    color: root.textTertiary
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    font.bold: true
    font.letterSpacing: 1.0
  }

  Column {
    id: modelColumn
    anchors.top: modelHeading.bottom
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.topMargin: Style.space(4)
    spacing: 0

    Repeater {
      model: root.models
      delegate: Item {
        property var modelRow: modelData
        width: modelColumn.width
        height: Style.space(26)

        Text {
          anchors.left: parent.left
          anchors.verticalCenter: parent.verticalCenter
          anchors.right: modelCost.left
          anchors.rightMargin: Style.space(10)
          textFormat: Text.PlainText
          text: modelRow.label
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
          elide: Text.ElideRight
        }
        Text {
          id: modelCost
          anchors.right: parent.right
          anchors.verticalCenter: parent.verticalCenter
          textFormat: Text.PlainText
          text: root.money(modelRow.costUsd) + "  ·  " + modelRow.messages
            + (modelRow.messages === 1 ? " message" : " messages")
          color: root.textSecondary
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          elide: Text.ElideLeft
        }
      }
    }
  }

  Text {
    id: sessionHeading
    anchors.top: modelColumn.bottom
    anchors.left: parent.left
    anchors.topMargin: Style.space(26)
    textFormat: Text.PlainText
    text: "CONVERSATIONS"
    color: root.textTertiary
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    font.bold: true
    font.letterSpacing: 1.0
  }

  ListView {
    id: sessionList
    anchors.top: sessionHeading.bottom
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.bottom: footer.top
    anchors.topMargin: Style.space(4)
    anchors.bottomMargin: Style.space(6)
    clip: true
    model: root.sessions
    currentIndex: root.selected
    boundsBehavior: Flickable.StopAtBounds

    delegate: Item {
      property var row: modelData
      width: sessionList.width
      height: Style.space(46)

      // Only the keyboard selection fills a row. The conversation you are in is a
      // thin accent bar on the left: two tinted rows at once read as two selections.
      Rectangle {
        anchors.fill: parent
        visible: index === root.selected
        color: root.selectedFill
      }
      Rectangle {
        anchors.left: parent.left
        anchors.verticalCenter: parent.verticalCenter
        width: Style.space(2)
        height: Math.round(parent.height * 0.55)
        radius: width / 2
        visible: row.current
        color: root.accent
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
        anchors.right: rowCost.left
        anchors.rightMargin: Style.space(10)
        anchors.top: parent.top
        anchors.topMargin: Style.space(6)
        textFormat: Text.PlainText
        text: row.title
        color: row.current ? root.accent : root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
        elide: Text.ElideRight
      }
      Text {
        anchors.left: parent.left
        anchors.right: rowCost.left
        anchors.rightMargin: Style.space(10)
        anchors.top: parent.top
        anchors.topMargin: Style.space(23)
        textFormat: Text.PlainText
        text: (row.when ? row.when + "  ·  " : "") + (row.models ? row.models : "no usage")
        color: root.textTertiary
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        elide: Text.ElideRight
      }
      Text {
        id: rowCost
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        textFormat: Text.PlainText
        text: root.money(row.costUsd)
        color: row.current ? root.accent : root.textSecondary
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
        font.bold: row.current
      }
      MouseArea {
        anchors.fill: parent
        cursorShape: Qt.PointingHandCursor
        onClicked: {
          root.selected = index
          root.forceActiveFocus()
        }
      }
    }

    Text {
      anchors.centerIn: parent
      width: parent.width * 0.8
      visible: root.sessions.length === 0 && !root.loading && root.error.length === 0
      textFormat: Text.PlainText
      text: "No usage recorded yet."
      color: root.textSecondary
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      wrapMode: Text.Wrap
      horizontalAlignment: Text.AlignHCenter
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
      text: "R REFRESH  ·  ↑↓ SCROLL  ·  ESC CLOSE"
      color: root.textTertiary
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      font.bold: true
      font.letterSpacing: 0.8
    }

    // The hint on the left already says "R REFRESH"; a second button for the same
    // action on the right was one control too many.
  }
}
