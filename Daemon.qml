import QtQuick
import Quickshell.Io

// A tiny JSON-lines client for omassistantd, for the calls the card makes
// itself (acts and grants). The CLI stays the human/debug path; this is the
// same protocol without spawning a process per click.
//
// One request is one line; the daemon answers one line with the same id. Since
// the daemon also allows a `once` grant check, the card mints consent through
// this same socket after the person confirms.
QtObject {
  id: root

  property string socketPath: ""
  readonly property bool connected: socket.connected
  // id -> callback(ok, result, error)
  property var pending: ({})
  property int serial: 0

  signal connectionChanged()

  function call(method, params, callback) {
    if (!socket.connected) {
      // Never queue a callback on a dead socket: the poll would grow forever.
      if (typeof callback === "function")
        callback(false, null, { code: "unavailable", message: "the broker is not connected" })
      if (socketPath.length > 0) {
        // Ask the socket to try again; Quickshell reconnects on this property.
        socket.connected = false
        socket.connected = true
      }
      return
    }
    var id = "c" + (serial += 1)
    var next = {}
    for (var key in pending) next[key] = pending[key]
    next[id] = callback
    pending = next
    socket.write(JSON.stringify({ id: id, method: method, params: params || {} }) + "\n")
  }

  function handleLine(line) {
    var record = null
    try {
      record = JSON.parse(String(line || ""))
    } catch (error) {
      return
    }
    if (!record || typeof record.id !== "string") return
    var callback = pending[record.id]
    if (typeof callback !== "function") return
    var next = {}
    for (var key in pending) if (key !== record.id) next[key] = pending[key]
    pending = next
    callback(record.ok === true, record.result || null, record.error || null)
  }

  function failAll(message) {
    var callbacks = pending
    pending = ({})
    for (var key in callbacks) {
      if (typeof callbacks[key] === "function") callbacks[key](false, null, { code: "unavailable", message: message })
    }
  }

  property Socket socket: Socket {
    path: root.socketPath
    connected: root.socketPath.length > 0
    parser: SplitParser {
      onRead: function(line) {
        if (line.length > 0) root.handleLine(line)
      }
    }
    onConnectionStateChanged: root.connectionChanged()
  }
}
