import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui

// The model settings sheet. It is loaded lazily by ChatPanel, fills the card's
// content area, and never talks to the host except through its signals: the
// overlay owns the config and the agent.
//
// This page is deliberately plain language. "Fast" and "Best" are the only
// routing words; everything technical (providers, context windows, API keys)
// either stays in pi or is a small tag. Keyboard first: arrows move, Enter
// chooses, Esc backs out.
FocusScope {
  id: root
  objectName: "omassistantSettings"

  property var state: ({})
  property bool locked: false
  property string mode: "main"      // main | picker
  property string pickRole: ""      // default | fast | best | local
  property int selected: 0
  property int pickIndex: 0

  signal close()
  signal chooseModel(string role, string provider, string id)
  signal setAuto(bool on)
  signal setLocalOnly(bool on)
  signal setDesktopWidget(bool on)
  signal openCosts()
  signal discover()
  signal refresh()

  readonly property var st: root.state && typeof root.state === "object" ? root.state : ({})
  readonly property var models: root.st.models ? root.st.models : []
  readonly property var discovery: root.st.discovery ? root.st.discovery : ({ state: "idle", message: "" })
  readonly property string noticeMessage: {
    // Searching/adding is already written on the Find row; do not repeat it.
    if ((root.discovery.state === "none" || root.discovery.state === "added"
         || root.discovery.state === "error") && String(root.discovery.message || "").length)
      return String(root.discovery.message)
    if (root.modelsLoading && root.models.length === 0) return "Looking for the models pi can use…"
    if (!root.modelsLoading && root.models.length === 0)
      return "No models yet. Set up pi once in a terminal: run pi, then /login. Omassistant uses whatever pi can reach."
    return ""
  }
  readonly property color noticeColor: root.discovery.state === "error" ? root.urgent : root.textSecondary
  readonly property bool busy: root.st.busy === true
  readonly property bool modelsLoading: root.st.modelsLoading === true
  readonly property var current: root.st.current ? root.st.current : ({})

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
  readonly property int sectionHeight: Style.space(30)
  readonly property int pickerRowHeight: Style.space(42)

  function rowHeightOf(row) {
    return row && row.kind === "section" ? root.sectionHeight : root.rowHeight
  }

  function isSelectable(row) {
    return !!row && (row.kind === "action" || row.kind === "toggle")
  }

  function firstSelectable() {
    for (var i = 0; i < root.rows.length; i++) if (root.isSelectable(root.rows[i])) return i
    return -1
  }

  function move(delta) {
    if (!root.rows.length) return
    var index = root.selected
    for (var step = 0; step < root.rows.length; step++) {
      index = (index + delta + root.rows.length) % root.rows.length
      if (root.isSelectable(root.rows[index])) {
        root.selected = index
        root.ensureVisible()
        return
      }
    }
  }

  function ensureVisible() {
    if (root.selected < 0) return
    var y = root.noticeMessage.length ? Style.space(40) : 0
    for (var i = 0; i < root.selected; i++) y += root.rowHeightOf(root.rows[i])
    var h = root.rowHeightOf(root.rows[root.selected])
    var top = mainFlick.contentY
    var bottom = top + mainFlick.height
    if (y < top) mainFlick.contentY = Math.max(0, y)
    else if (y + h > bottom) {
      mainFlick.contentY = Math.min(Math.max(0, mainFlick.contentHeight - mainFlick.height),
        y + h - mainFlick.height)
    }
  }

  function clampSelection() {
    if (!root.rows.length) { root.selected = -1; return }
    if (root.isSelectable(root.rows[root.selected])) return
    root.selected = root.firstSelectable()
  }

  readonly property var rows: {
    var s = root.st
    var routingOn = s.auto === true && s.autoAvailable === true
    var list = []
    list.push({ kind: "section", label: "MODEL" })

    if (!routingOn) {
      list.push({
        kind: "action", key: "default", role: "default",
        label: "Model", detail: "Used when automatic choice is off.",
        value: s.default ? s.default.name : (root.modelsLoading ? "Loading…" : "Not set"),
        enabled: !!s.default && !root.locked
      })
    }

    list.push({
      kind: "toggle", key: "auto",
      label: "Automatic model choice",
      detail: s.auto === true
        ? (s.fast && s.best && s.fast.key === s.best.key
          ? "Fast and Best are the same model. Pick a different Fast model to see a change."
          : "Fast for short questions · Best for form/deck, visual and desktop work.")
        : (s.autoAvailable ? "Off — one model does everything."
          : "Add a second model to choose per request."),
      value: s.auto === true ? "ON" : "OFF",
      on: s.auto === true,
      // With one model it has no effect, but it can still be turned off.
      enabled: !root.locked && (s.autoAvailable === true || s.auto === true)
    })

    if (routingOn) {
      list.push({
        kind: "action", key: "fast", role: "fast",
        label: "Fast model",
        detail: "Short questions. A free or local model fits here.",
        value: s.fast ? s.fast.name : "Not set",
        enabled: !!s.fast && !root.locked
      })
      list.push({
        kind: "action", key: "best", role: "best",
        label: "Best model",
        detail: "Longer, visual or desktop requests.",
        value: s.best ? s.best.name : "Not set",
        enabled: !!s.best && !root.locked
      })
    }

    list.push({
      kind: "action", key: "local", role: "local",
      label: "On this computer",
      detail: s.local ? "Runs locally · nothing leaves the machine."
        : "No local model yet. Find one below.",
      value: s.local ? s.local.name : "Find",
      enabled: !root.locked
    })
    list.push({
      kind: "toggle", key: "localOnly",
      label: "Keep everything on this computer",
      detail: s.localOnly === true
        ? "Only the local model is used. Cloud models are not contacted."
        : (s.localAvailable ? "Off — cloud models are allowed."
          : "Add a local model first."),
      value: s.localOnly === true ? "ON" : "OFF",
      on: s.localOnly === true,
      enabled: s.localAvailable === true && !root.locked
    })

    list.push({
      kind: "action", key: "find",
      label: "Find models on this computer",
      detail: root.discovery.state === "searching"
        ? root.discovery.message
        : (root.discovery.state === "adding"
          ? root.discovery.message
          : "Looks for Ollama, LM Studio and llama.cpp on this machine."),
      value: root.discovery.state === "added" ? "Again" : "Find",
      enabled: !root.locked && root.discovery.state !== "searching" && root.discovery.state !== "adding"
    })

    list.push({ kind: "section", label: "DESKTOP" })
    list.push({
      kind: "toggle", key: "desktopWidget",
      label: "Companion card",
      detail: "The small card in the bottom-right corner. The bar diamond stays.",
      value: s.desktopWidget === true ? "ON" : "OFF",
      on: s.desktopWidget === true,
      enabled: !root.locked
    })

    list.push({ kind: "section", label: "USAGE" })
    list.push({
      kind: "action", key: "costs",
      label: "Usage & costs",
      detail: "Total, per conversation and per model, in USD.",
      value: "Open",
      enabled: !root.locked
    })

    list.push({ kind: "section", label: "ABOUT" })
    list.push({
      kind: "info", label: "Answering with",
      detail: root.current.name ? root.current.name : "—",
      value: root.busy ? "working" : ""
    })
    list.push({
      kind: "info", label: "Local broker",
      detail: root.st.broker === true ? "Connected" : "Not running",
      value: ""
    })
    list.push({
      kind: "info", label: "Version",
      detail: root.st.version && String(root.st.version).length ? String(root.st.version) : "—",
      value: ""
    })
    return list
  }

  onRowsChanged: root.clampSelection()
  onPickerModelsChanged: {
    if (!root.pickerModels.length) root.pickIndex = 0
    else if (root.pickIndex >= root.pickerModels.length) root.pickIndex = root.pickerModels.length - 1
  }

  // The role whose models the picker is showing.
  readonly property var pickerModels: {
    var role = root.pickRole
    var list = []
    for (var i = 0; i < root.models.length; i++) {
      var m = root.models[i]
      if (role === "local" && m.local !== true) continue
      list.push(m)
    }
    return list
  }

  readonly property var pickerRoleModel: {
    if (root.pickRole === "default") return root.st.default || null
    return root.st[root.pickRole] || null
  }

  function tagText(tags) {
    if (!tags || typeof tags.length !== "number") return ""
    var out = []
    for (var i = 0; i < tags.length; i++) out.push(String(tags[i]))
    return out.join(" · ")
  }

  function pickerTitle() {
    if (root.pickRole === "fast") return "FAST MODEL"
    if (root.pickRole === "best") return "BEST MODEL"
    if (root.pickRole === "local") return "ON THIS COMPUTER"
    return "MODEL"
  }

  function pickerDetail() {
    if (root.pickRole === "fast") return "Used for short questions. A free or local model is a good fit."
    if (root.pickRole === "best") return "Used for longer, visual or desktop requests."
    if (root.pickRole === "local") return "Runs on this computer. Nothing is sent to a cloud model."
    return "Used when automatic choice is off."
  }

  function openPicker(role) {
    root.pickRole = role
    root.mode = "picker"
    var wanted = root.st[role] ? root.st[role].key : ""
    var index = 0
    for (var i = 0; i < root.pickerModels.length; i++) {
      if (root.pickerModels[i].key === wanted) { index = i; break }
    }
    root.pickIndex = Math.min(index, Math.max(0, root.pickerModels.length - 1))
    Qt.callLater(function() {
      if (root.pickerModels.length) pickerList.positionViewAtIndex(root.pickIndex, ListView.Contain)
    })
  }

  function closePicker() {
    var role = root.pickRole
    root.mode = "main"
    for (var i = 0; i < root.rows.length; i++) {
      if (root.rows[i].key === role) { root.selected = i; return }
    }
    root.selected = root.firstSelectable()
    root.ensureVisible()
  }

  function choose() {
    if (root.mode === "picker") {
      if (root.locked || !root.pickerModels.length) return
      var model = root.pickerModels[Math.max(0, Math.min(root.pickIndex, root.pickerModels.length - 1))]
      if (!model) return
      root.chooseModel(root.pickRole, model.provider, model.id)
      root.closePicker()
      return
    }
    var row = root.rows[root.selected]
    if (!row || !row.enabled) return
    if (row.kind === "toggle") {
      if (row.key === "auto") root.setAuto(row.on !== true)
      else if (row.key === "localOnly") root.setLocalOnly(row.on !== true)
      else if (row.key === "desktopWidget") root.setDesktopWidget(row.on !== true)
      return
    }
    if (row.kind === "action") {
      if (row.key === "costs") root.openCosts()
      else if (row.key === "find") root.discover()
      else if (row.key === "local" && !root.st.localAvailable) root.discover()
      else if (row.role) root.openPicker(row.role)
    }
  }

  function movePicker(delta) {
    if (!root.pickerModels.length) return
    root.pickIndex = (root.pickIndex + delta + root.pickerModels.length) % root.pickerModels.length
    pickerList.positionViewAtIndex(root.pickIndex, ListView.Contain)
  }

  Keys.onPressed: function(event) {
    if (event.key === Qt.Key_Escape) {
      if (root.mode === "picker") root.closePicker()
      else root.close()
      event.accepted = true
      return
    }
    if (root.mode === "picker") {
      if (event.key === Qt.Key_Up) { root.movePicker(-1); event.accepted = true }
      else if (event.key === Qt.Key_Down) { root.movePicker(1); event.accepted = true }
      else if (event.key === Qt.Key_Left || event.key === Qt.Key_Backspace) {
        root.closePicker(); event.accepted = true
      } else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
        root.choose(); event.accepted = true
      }
      return
    }
    if (event.key === Qt.Key_Up) { root.move(-1); event.accepted = true }
    else if (event.key === Qt.Key_Down) { root.move(1); event.accepted = true }
    else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter
             || event.key === Qt.Key_Space) {
      root.choose(); event.accepted = true
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
    id: settingsHeader
    anchors.top: parent.top
    anchors.left: parent.left
    anchors.right: parent.right
    height: root.mode === "picker" ? Style.space(54) : Style.space(28)

    Text {
      anchors.left: parent.left
      anchors.top: parent.top
      textFormat: Text.PlainText
      text: root.mode === "picker" ? root.pickerTitle() : "SETTINGS"
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
      text: root.mode === "picker" ? "ESC BACK" : "ESC CLOSE"
      color: root.textTertiary
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      font.bold: true
      font.letterSpacing: 1.0
      MouseArea {
        anchors.fill: parent
        cursorShape: Qt.PointingHandCursor
        onClicked: {
          if (root.mode === "picker") root.closePicker()
          else root.close()
        }
      }
    }

    Text {
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.top: parent.top
      anchors.topMargin: Style.space(18)
      visible: root.mode === "picker"
      textFormat: Text.PlainText
      text: root.pickerDetail()
      color: root.textSecondary
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      wrapMode: Text.Wrap
      elide: Text.ElideRight
      maximumLineCount: 2
    }

    Rectangle {
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.bottom: parent.bottom
      height: 1
      color: root.separator
    }
  }

  Rectangle {
    id: lockedBar
    anchors.top: settingsHeader.bottom
    anchors.left: parent.left
    anchors.right: parent.right
    height: root.locked ? Style.space(22) : 0
    visible: root.locked
    color: "transparent"
    Text {
      anchors.left: parent.left
      anchors.verticalCenter: parent.verticalCenter
      textFormat: Text.PlainText
      text: "FINISH THE CURRENT ANSWER TO CHANGE MODELS"
      color: root.urgent
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      font.bold: true
      font.letterSpacing: 0.8
    }
  }

  Flickable {
    id: mainFlick
    visible: root.mode === "main"
    anchors.top: lockedBar.bottom
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.bottom: settingsFooter.top
    anchors.topMargin: Style.space(10)
    anchors.bottomMargin: Style.space(6)
    clip: true
    contentWidth: width
    contentHeight: mainColumn.implicitHeight
    boundsBehavior: Flickable.StopAtBounds

    Column {
      id: mainColumn
      width: mainFlick.width
      spacing: 0

      Item {
        width: mainColumn.width
        height: root.noticeMessage.length ? Style.space(40) : 0
        visible: root.noticeMessage.length > 0

        Text {
          anchors.left: parent.left
          anchors.right: parent.right
          anchors.top: parent.top
          anchors.topMargin: Style.space(8)
          textFormat: Text.PlainText
          text: root.noticeMessage
          color: root.noticeColor
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          wrapMode: Text.Wrap
          maximumLineCount: 2
          elide: Text.ElideRight
        }
      }

      Repeater {
        model: root.rows
        delegate: Item {
          id: rowItem
          property var row: modelData
          width: mainColumn.width
          height: root.rowHeightOf(row)
          opacity: row.enabled === false ? 0.5 : 1.0

          Rectangle {
            anchors.fill: parent
            visible: index === root.selected && root.isSelectable(row)
            color: root.selectedFill
          }

          Rectangle {
            anchors.bottom: parent.bottom
            anchors.left: parent.left
            anchors.right: parent.right
            height: 1
            color: root.separator
            visible: row.kind !== "section"
          }

          Text {
            anchors.left: parent.left
            anchors.right: rowValue.left
            anchors.rightMargin: Style.space(10)
            anchors.bottom: parent.verticalCenter
            anchors.bottomMargin: Style.space(1)
            textFormat: Text.PlainText
            text: row.label
            color: row.enabled === false ? root.textTertiary : root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            font.bold: row.kind === "section"
            font.letterSpacing: row.kind === "section" ? 1.2 : 0
            elide: Text.ElideRight
          }

          Text {
            id: rowDetail
            anchors.left: parent.left
            anchors.right: rowValue.left
            anchors.rightMargin: Style.space(10)
            anchors.top: parent.verticalCenter
            anchors.topMargin: Style.space(2)
            visible: row.kind !== "section"
            textFormat: Text.PlainText
            text: String(row.detail || "")
            color: root.textTertiary
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            elide: Text.ElideRight
            maximumLineCount: 1
          }

          Text {
            id: rowValue
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            visible: row.kind !== "section" && String(row.value || "").length > 0
            textFormat: Text.PlainText
            text: String(row.value || "")
            color: row.kind === "toggle" || row.kind === "action" ? root.accent : root.textSecondary
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
            font.bold: row.kind === "toggle"
            horizontalAlignment: Text.AlignRight
            elide: Text.ElideLeft
            width: Math.min(implicitWidth, parent.width * 0.48)
          }

          MouseArea {
            anchors.fill: parent
            enabled: root.isSelectable(row)
            cursorShape: enabled ? Qt.PointingHandCursor : Qt.ArrowCursor
            onClicked: {
              root.selected = index
              root.forceActiveFocus()
              if (row.enabled !== false) root.choose()
            }
          }
        }
      }

    }
  }

  ListView {
    id: pickerList
    visible: root.mode === "picker"
    anchors.top: settingsHeader.bottom
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.bottom: settingsFooter.top
    anchors.topMargin: Style.space(12)
    anchors.bottomMargin: Style.space(6)
    clip: true
    model: root.pickerModels
    currentIndex: root.pickIndex
    boundsBehavior: Flickable.StopAtBounds

    delegate: Item {
      property var modelRow: modelData
      width: pickerList.width
      height: root.pickerRowHeight

      Rectangle {
        anchors.fill: parent
        visible: index === root.pickIndex
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
        anchors.right: modelDetail.left
        anchors.rightMargin: Style.space(10)
        anchors.verticalCenter: parent.verticalCenter
        textFormat: Text.PlainText
        text: modelRow.name
        color: index === root.pickIndex ? root.accent : root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
        elide: Text.ElideRight
      }

      Text {
        id: modelDetail
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        width: Math.min(implicitWidth, parent.width * 0.46)
        textFormat: Text.PlainText
        text: {
          var bits = [modelRow.providerLabel]
          if (modelRow.tags && modelRow.tags.length) bits.push(root.tagText(modelRow.tags))
          if (root.pickerRoleModel && root.pickerRoleModel.key === modelRow.key) bits.push("SELECTED")
          return bits.join("  ·  ")
        }
        color: root.textTertiary
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        horizontalAlignment: Text.AlignRight
        elide: Text.ElideLeft
      }

      MouseArea {
        anchors.fill: parent
        cursorShape: Qt.PointingHandCursor
        onClicked: {
          root.pickIndex = index
          root.forceActiveFocus()
          if (!root.locked) root.choose()
        }
      }
    }
  }

  Item {
    anchors.top: settingsHeader.bottom
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.bottom: settingsFooter.top
    visible: root.mode === "picker" && root.pickerModels.length === 0

    Text {
      anchors.centerIn: parent
      width: parent.width * 0.82
      textFormat: Text.PlainText
      text: {
        if (root.pickRole === "local")
          return "No local models yet. Start Ollama or LM Studio, then find them here."
        return root.modelsLoading ? "Loading models…" : "pi has no models for this provider yet."
      }
      color: root.textSecondary
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      wrapMode: Text.Wrap
      horizontalAlignment: Text.AlignHCenter
    }

    Row {
      anchors.horizontalCenter: parent.horizontalCenter
      anchors.top: parent.verticalCenter
      anchors.topMargin: Style.space(18)
      spacing: Style.space(22)

      Text {
        textFormat: Text.PlainText
        text: "FIND MODELS"
        color: root.accent
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
        font.bold: true
        font.letterSpacing: 1.0
        MouseArea {
          anchors.fill: parent
          anchors.margins: -Style.space(6)
          cursorShape: Qt.PointingHandCursor
          onClicked: root.discover()
        }
      }

      Text {
        textFormat: Text.PlainText
        text: "REFRESH"
        color: root.textSecondary
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
        font.bold: true
        font.letterSpacing: 1.0
        MouseArea {
          anchors.fill: parent
          anchors.margins: -Style.space(6)
          cursorShape: Qt.PointingHandCursor
          onClicked: root.refresh()
        }
      }
    }
  }

  Item {
    id: settingsFooter
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.bottom: parent.bottom
    height: Style.space(20)

    Text {
      anchors.left: parent.left
      anchors.verticalCenter: parent.verticalCenter
      textFormat: Text.PlainText
      text: root.mode === "picker" ? "↑↓ CHOOSE  ·  ENTER SELECT  ·  ESC BACK"
        : "↑↓ MOVE  ·  ENTER SELECT  ·  ESC CLOSE"
      color: root.textTertiary
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      font.bold: true
      font.letterSpacing: 1.0
    }
  }
}
