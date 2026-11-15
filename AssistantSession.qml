import QtQuick
import Quickshell.Io

// One long-lived `pi --mode rpc` child, spoken to in JSONL over stdin/stdout.
//
// The protocol is strictly line-framed JSON: commands in, responses plus
// session events out (see docs/rpc.md in the installed pi). This object owns
// nothing visual — it reports what the agent said and what went wrong, and the
// overlay decides how to say it in words a person can act on.
//
// Security stance for M0: the runtime is started with every tool and extension
// disabled, so the assistant can answer but cannot act. Capabilities arrive
// later through omassistantd's broker, never through pi flags.
QtObject {
  id: root

  // Ordinary JSONL stdout is a pipe; a wedged child is worse than a slow one.
  // Restart backoff is bounded so a missing binary cannot spin.
  property string executable: "setpriv"
  property var argv: []
  property string workspace: ""
  property bool autoRestart: true
  // Where pi keeps conversations, and the one to open ("" = a fresh session).
  // Both are appended by start(), so a switch is a child replacement and never
  // a half-applied flag.
  property string sessionDir: ""
  property string requestedSession: ""
  // Reported by get_state; the host indexes these for the history picker.
  property string currentSessionFile: ""
  property string currentSessionId: ""
  property int messageCount: 0

  // True while the child has not printed a single protocol line.
  property bool sawOutput: false
  readonly property bool running: proc.running
  // Handshake completed: get_state answered. Only then is pi worth talking to.
  property bool live: false
  property bool busy: false
  property string modelLabel: ""
  // The model pi is actually using, from get_state or the last set_model reply.
  property string activeProvider: ""
  property string activeModelId: ""
  property string activeModelName: ""
  property string activeThinking: ""
  // One in-flight model switch: set_model, then the best supported thinking
  // level. The host waits for the callback before sending the prompt.
  property var pendingModel: null
  property string pendingModelOp: ""
  property string pendingThinkingLevel: ""
  property string faultText: ""
  property int failures: 0

  // Streamed answer text, already concatenated.
  property string answer: ""
  // The last provider-level error message, reported once the run settles.
  property string runError: ""
  // A stream that runs away is cut here rather than in the shell's heap.
  readonly property int answerLimit: 262144
  property bool answerTruncated: false
  // A run that has gone quiet long enough to say so (still busy, just waiting).
  property bool waiting: false
  property int stallStage: 0
  // Why a wedged child was replaced, worded for the transcript instead of the
  // generic "pi stopped unexpectedly".
  property string forcedReason: ""
  // First stderr line of a startup that died, already redacted: pi says things
  // like `Error: Model "x" not found` there, which is exactly what the person
  // needs to read. Only used when the child never got as far as a protocol line.
  property string lastStderr: ""

  // Emitted for every text delta of the answer in flight.
  signal output(string text)
  // Exactly once per accepted prompt: ok=false means do not trust the answer;
  // interrupted means the person stopped it on purpose, which is not a fault.
  signal answered(bool ok, string detail, bool interrupted)
  // Child went away while idle; detail is a plain sentence for the transcript.
  signal died(string detail)
  // Header state (live/busy/fault) changed.
  signal stateChanged()
  // The child is up and told us which session it is in.
  signal sessionReady(string file, string id, int messageCount)
  // A get_messages reply: the conversation to rebuild the transcript from.
  signal history(var messages)
  // The configured models, as pi reports them (the host sanitizes and shows).
  signal modelsReady(var models)
  // The model actually answering changed (a route switch, a session restore).
  signal modelChanged(string provider, string id, string name, string thinking)

  property int serial: 0
  property string queuedPrompt: ""
  // One image attached to the question in flight (observe.screen's payload).
  property var queuedImage: null

  function send(record) {
    if (!proc.running) return false
    proc.write(JSON.stringify(record) + "\n")
    return true
  }

  function start() {
    // A restart already has an exit handler waiting to start the replacement;
    // starting here would race the child that is still shutting down.
    if (proc.running || pendingRestart) return
    faultText = ""
    lastStderr = ""
    live = false
    sawOutput = false
    runError = ""
    var args = argv.slice()
    if (sessionDir.length) args.push("--session-dir", sessionDir)
    if (requestedSession.length) args.push("--session", requestedSession)
    proc.command = [executable].concat(args)
    proc.running = true
    startWatchdog.restart()
  }

  function stop() {
    restartTimer.stop()
    startWatchdog.stop()
    stallWatchdog.stop()
    abortEscalation.stop()
    killTimer.stop()
    intentionalStop = true
    if (proc.running) proc.running = false
  }

  // A person pressed Retry after a startup failure. Forget the old failure
  // count and any fault text so the card returns to "connecting", not
  // "offline", while the new child starts.
  function retryNow() {
    failures = 0
    faultText = ""
    lastStderr = ""
    if (proc.running) restart()
    else start()
  }

  property bool intentionalStop: false

  // Rebuild the child when the configuration it was started with changed.
  // A child that will not exit must not be able to latch pendingRestart: arm
  // the same kill escalation the stall path uses.
  function restart() {
    // A deliberate restart (new conversation, a session switch, new config) is
    // a fresh start and must not inherit the previous failure count. A run in
    // flight cannot settle after the child is gone, so it is closed here — the
    // host consumes the interrupted answer.
    failures = 0
    if (busy) {
      busy = false
      waiting = false
      stallWatchdog.stop()
      abortEscalation.stop()
      answer = ""
      runError = ""
      answered(false, "", true)
    }
    if (proc.running) {
      pendingRestart = true
      armKill(proc.processId)
      proc.running = false
    } else {
      start()
    }
  }

  // SIGKILL only the process we armed for, never a replacement that started
  // in the meantime.
  property int killPid: 0

  function armKill(pid) {
    var id = Number(pid)
    // Without a real pid there is nothing safe to aim SIGKILL at later.
    if (!(id > 0)) return
    killPid = id
    killTimer.restart()
  }

  property bool pendingRestart: false

  // Send a question to the agent. Queued until the handshake completes, so the
  // first question of a fresh shell never races the child's startup.
  function ask(text, image) {
    if (busy) return false
    if (!String(text).length) return false
    busy = true
    answer = ""
    runError = ""
    answerTruncated = false
    lastStderr = ""
    waiting = false
    stallStage = 0
    forcedReason = ""
    stallWatchdog.restart()
    queuedPrompt = String(text)
    queuedImage = image && typeof image === "object" && typeof image.data === "string" ? image : null
    if (live) flushQueued()
    else if (!proc.running) start()
    stateChanged()
    return true
  }

  // Ask pi to cancel, but do not trust it to answer: a provider call wedged on
  // the network can swallow the abort too. If nothing settles, the child is
  // replaced — an overlay that can never ask again is worse than one honest
  // restart.
  function abort() {
    if (!busy) return
    send({ id: "abort-" + (serial += 1), type: "abort" })
    abortEscalation.restart()
  }

  function abandon(reason) {
    forcedReason = reason
    if (!proc.running) return
    // Terminate first so pi can dispose its runtime; a child that is wedged
    // hard enough to ignore SIGTERM gets SIGKILL three seconds later.
    armKill(proc.processId)
    proc.signal(15)
  }

  // Stop the run in flight (if any) and rebuild the child. The caller decides
  // which session it lands in: newSession() clears requestedSession first.
  function reset() {
    queuedPrompt = ""
    queuedImage = null
    answer = ""
    runError = ""
    forcedReason = ""
    failures = 0
    waiting = false
    stallStage = 0
    if (busy) {
      resetPending = true
      abort()
      return
    }
    restart()
  }

  property bool resetPending: false

  function flushQueued() {
    if (!queuedPrompt.length) return
    var text = queuedPrompt
    queuedPrompt = ""
    var id = "q" + (serial += 1)
    var record = { id: id, type: "prompt", message: text }
    if (queuedImage) {
      record.images = [{ type: "image", data: queuedImage.data,
        mimeType: queuedImage.mimeType || "image/jpeg" }]
    }
    queuedImage = null
    if (!send(record)) {
      busy = false
      answered(false, "The assistant is not running, so that question was not sent.", false)
      return
    }
    stateChanged()
  }

  function requestState() {
    send({ id: "state-" + (serial += 1), type: "get_state" })
  }

  // Ask for the conversation the child is carrying (opening, or after a switch).
  function requestHistory() {
    if (!live) return false
    return send({ id: "hist-" + (serial += 1), type: "get_messages" })
  }

  // Ask pi what it can use. The host calls this after the handshake and after a
  // restart; it is cheap and the catalog is what the settings page draws.
  function requestModels() {
    if (!live) return false
    return send({ id: "models-" + (serial += 1), type: "get_available_models" })
  }

  // Switch the one child to another model, then set the best thinking level it
  // supports. callback(ok, detail) runs exactly once; the host sends the prompt
  // only after it, so set_model and prompt can never race.
  function setModel(provider, modelId, thinking, callback) {
    var request = {
      provider: String(provider || ""),
      modelId: String(modelId || ""),
      thinking: String(thinking || ""),
      callback: typeof callback === "function" ? callback : null
    }
    if (!request.provider.length || !request.modelId.length) {
      if (request.callback) request.callback(false, "the model has no provider or id")
      return false
    }
    if (live && request.provider === activeProvider && request.modelId === activeModelId
        && (!request.thinking.length || request.thinking === activeThinking)) {
      if (request.callback) request.callback(true, "")
      return true
    }
    if (!live) {
      pendingModel = request
      return true
    }
    return applyModel(request)
  }

  function applyModel(request) {
    pendingModel = request
    var id = "model-" + (serial += 1)
    pendingModelOp = id
    if (!send({ id: id, type: "set_model", provider: request.provider, modelId: request.modelId })) {
      finishModel(false, "the assistant is not running")
      return false
    }
    return true
  }

  function finishModel(ok, detail) {
    var request = pendingModel
    pendingModel = null
    pendingModelOp = ""
    pendingThinkingLevel = ""
    if (request && request.callback) request.callback(!!ok, String(detail || ""))
  }

  function setActiveModel(provider, modelId, name, thinking) {
    var nextProvider = String(provider || "")
    var nextId = String(modelId || "")
    var nextName = String(name || modelId || "")
    var nextThinking = thinking === undefined ? activeThinking : String(thinking || "")
    var changed = nextProvider !== activeProvider || nextId !== activeModelId
      || nextName !== activeModelName || nextThinking !== activeThinking
    activeProvider = nextProvider
    activeModelId = nextId
    activeModelName = nextName
    activeThinking = nextThinking
    modelLabel = nextName || nextId
    if (changed) {
      modelChanged(activeProvider, activeModelId, activeModelName, activeThinking)
      stateChanged()
    }
  }

  // The level closest to what the route asked for, among those the model says
  // it supports. Asking for "high" on a model with off/low must not fail the
  // switch; choosing a supported level is enough.
  function closestThinking(levels, wanted) {
    if (!Array.isArray(levels) || levels.length === 0) return ""
    var order = ["off", "minimal", "low", "medium", "high", "xhigh", "max"]
    var want = String(wanted || "")
    if (levels.indexOf(want) >= 0) return want
    var wi = order.indexOf(want)
    if (wi < 0) return String(levels[0])
    var best = String(levels[0])
    var bestDistance = 999
    for (var i = 0; i < levels.length; i++) {
      var index = order.indexOf(String(levels[i]))
      if (index < 0) continue
      var distance = Math.abs(index - wi)
      if (distance < bestDistance) {
        bestDistance = distance
        best = String(levels[i])
      }
    }
    return best
  }

  // Open another conversation. A child replacement is the only reliable switch:
  // pi's in-process session commands can be cancelled, and a prompt written
  // immediately after one is accepted but never runs (see NEW_SESSION note).
  function resumeSession(file) {
    requestedSession = String(file || "")
    restart()
  }

  function newSession() {
    requestedSession = ""
    reset()
  }

  // Provider errors can quote a request URL, a header or a whole HTML body.
  // Keep one short clause and strip anything shaped like a credential before it
  // can reach the transcript (which is drawn, and could be screenshotted).
  function redactSecrets(value) {
    var text = String(value)
    text = text.replace(/\b(sk|pk|rk)-[A-Za-z0-9_-]{6,}/g, "$1-[redacted]")
    text = text.replace(/(api[_-]?key|token|secret|password)\s*[=:]\s*[^\s&,"']+/gi, "$1=[redacted]")
    text = text.replace(/([?&](?:key|api_key|access_token|token)=)[^\s&"']+/gi, "$1[redacted]")
    text = text.replace(/Bearer\s+[A-Za-z0-9._~+/-]+=*/g, "Bearer [redacted]")
    return text
  }

  function plainError(value) {
    var text = redactSecrets(String(value === undefined || value === null ? "" : value))
      .replace(/\s+/g, " ").trim()
    if (!text.length) return "the assistant rejected it"
    if (text.length > 180) text = text.slice(0, 177) + "…"
    return text
  }

  function handleRecord(record) {
    var type = String(record.type || "")
    if (busy) stallWatchdog.restart()
    if (type === "response") {
      var command = String(record.command || "")
      if (command === "get_state") {
        if (record.success === false) return
        sawOutput = true
        if (faultText.length) faultText = ""
        startWatchdog.stop()
        var wasLive = live
        live = true
        var model = record.data && record.data.model ? record.data.model : null
        var data = record.data || ({})
        setActiveModel(model ? String(model.provider || "") : "",
          model ? String(model.id || "") : "",
          model ? String(model.name || model.id || "") : "",
          String(data.thinkingLevel || ""))
        currentSessionFile = String(data.sessionFile || "")
        currentSessionId = String(data.sessionId || "")
        messageCount = Number(data.messageCount || 0)
        sessionReady(currentSessionFile, currentSessionId, messageCount)
        if (pendingModel) applyModel(pendingModel)
        if (!wasLive) {
          stateChanged()
          // A model switch that was waiting for the handshake owns the next
          // prompt; flush only when none is pending.
          if (!pendingModel) flushQueued()
        }
        return
      }
      if (command === "get_available_models") {
        sawOutput = true
        if (record.success === false) {
          modelsReady([])
          return
        }
        var models = record.data && record.data.models ? record.data.models : []
        modelsReady(models)
        return
      }
      if (command === "set_model") {
        if (!pendingModel || String(record.id || "") !== pendingModelOp) return
        if (record.success === false) {
          finishModel(false, plainError(record.error))
          return
        }
        var applied = record.data && record.data.id ? record.data : null
        setActiveModel(applied ? String(applied.provider || "") : pendingModel.provider,
          applied ? String(applied.id || "") : pendingModel.modelId,
          applied ? String(applied.name || applied.id || "") : pendingModel.modelId,
          activeThinking)
        if (!pendingModel.thinking.length) {
          finishModel(true, "")
          return
        }
        send({ id: "think-" + (serial += 1), type: "get_available_thinking_levels" })
        return
      }
      if (command === "get_available_thinking_levels") {
        if (!pendingModel) return
        var levels = record.data && record.data.levels ? record.data.levels : []
        var level = closestThinking(levels, pendingModel.thinking)
        if (!level.length) {
          finishModel(true, "")
          return
        }
        pendingThinkingLevel = level
        send({ id: "think-set-" + (serial += 1), type: "set_thinking_level", level: level })
        return
      }
      if (command === "set_thinking_level") {
        if (!pendingModel) return
        var appliedLevel = String((record.data && record.data.level) || pendingThinkingLevel)
        if (record.success !== false) setActiveModel(activeProvider, activeModelId, activeModelName, appliedLevel)
        pendingThinkingLevel = ""
        finishModel(true, "")
        return
      }
      if (command === "get_messages") {
        if (record.success === false) {
          history([])
          return
        }
        var list = record.data && record.data.messages ? record.data.messages : []
        history(list)
        return
      }
      if (command === "prompt") {
        sawOutput = true
        if (record.success === false) {
          busy = false
          answered(false, "That question did not go through: " + plainError(record.error) + ".", false)
          return
        }
        var disposition = record.data && record.data.disposition ? String(record.data.disposition) : ""
        // "handled" means nothing will ever run, so do not wait for agent_settled.
        if (disposition === "handled") {
          busy = false
          answered(true, "", false)
        }
        return
      }
      if (command === "parse" && record.success === false) {
        busy = false
        answered(false, "The assistant could not read the question: " + plainError(record.error) + ".", false)
        return
      }
      return
    }

    if (type === "message_update") {
      sawOutput = true
      var update = record.assistantMessageEvent ? record.assistantMessageEvent : null
      if (update && String(update.type) === "text_delta" && typeof update.delta === "string") {
        if (answer.length < answerLimit) {
          answer += update.delta
          output(update.delta)
        } else if (!answerTruncated) {
          answerTruncated = true
        }
      }
      return
    }

    if (type === "message_end") {
      var message = record.message ? record.message : null
      if (message && String(message.role) === "assistant") {
        var reason = String(message.stopReason || "")
        if (reason === "error" && message.errorMessage) runError = plainError(message.errorMessage)
      }
      return
    }

    if (type === "agent_settled") {
      if (!busy) return
      stallWatchdog.stop()
      abortEscalation.stop()
      waiting = false
      busy = false
      var aborted = record.aborted === true
      var detail = ""
      if (runError.length) detail = runError
      else if (aborted) detail = answer.length > 0 ? "Stopped before the answer was finished." : "Stopped."
      else if (answerTruncated) detail = "The answer was cut off at 256 KB."
      // A clean settle is the only proof the child is healthy; a handshake is
      // not, and a crash loop must keep backing off because of it.
      if (!aborted && !runError.length) failures = 0
      forcedReason = ""
      answered(!aborted && !runError.length, detail, aborted)
      stateChanged()
      if (resetPending) {
        resetPending = false
        restart()
      }
      return
    }
  }

  function handleExit(exitCode) {
    if (intentionalStop) return
    restartTimer.stop()
    startWatchdog.stop()
    stallWatchdog.stop()
    abortEscalation.stop()
    killTimer.stop()
    waiting = false
    resetPending = false
    var wasBusy = busy
    var everRan = sawOutput
    live = false
    busy = false
    if (pendingModel) finishModel(false, "the assistant restarted before switching models")
    faultsFromExit(exitCode, everRan, wasBusy)
  }

  // A child that never printed a line did not start: say so in words that name
  // the thing to check, and stop retrying once it is clearly not our fault.
  function faultsFromExit(exitCode, everRan, wasBusy) {
    var detail
    if (!everRan) {
      if (lastStderr.length) {
        detail = "pi could not start: " + lastStderr
      } else {
        detail = "pi did not start. Check that it is installed, and the model and provider in "
          + "~/.config/omassistant/config.json."
      }
      if (exitCode > 0) detail += " (exit " + exitCode + ")"
    } else if (forcedReason.length) {
      detail = forcedReason
    } else {
      detail = "pi stopped unexpectedly"
      if (exitCode > 0) detail += " (exit " + exitCode + ")"
      detail += "."
    }
    forcedReason = ""

    if (wasBusy) answered(false, detail, false)
    else died(detail)

    if (!autoRestart) return
    failures += 1
    if ((!everRan && failures >= 3) || failures >= 8) {
      faultText = detail
      stateChanged()
      return
    }
    restartTimer.interval = Math.min(700 * failures, 4000)
    restartTimer.restart()
  }

  property Process proc: Process {
    stdinEnabled: true
    workingDirectory: root.workspace
    stdout: SplitParser {
      onRead: function(line) {
        if (!line.length) return
        root.sawOutput = true
        var record
        try {
          record = JSON.parse(line)
        } catch (error) {
          return
        }
        if (record && record.type) root.handleRecord(record)
      }
    }
    // stderr is diagnostics, not protocol. Keep only the first line of a
    // startup failure (redacted): `pi` puts the actionable sentence there. Once
    // a run is under way its stderr is not surfaced at all.
    stderr: SplitParser {
      onRead: function(line) {
        if (line.length && !root.sawOutput)
          root.lastStderr = root.redactSecrets(line).trim().slice(0, 200)
      }
    }
    onStarted: {
      root.sawOutput = false
      root.runError = ""
      root.lastStderr = ""
      root.requestState()
    }
    onExited: function(exitCode, exitStatus) {
      if (root.pendingRestart) {
        root.pendingRestart = false
        root.start()
        return
      }
      root.handleExit(exitCode)
    }
  }

  property Timer restartTimer: Timer {
    repeat: false
    onTriggered: root.start()
  }

  // Silence while a prompt is in flight. A minute with no token is worth
  // saying out loud; two minutes is worth replacing the child for.
  property Timer stallWatchdog: Timer {
    interval: 60000
    repeat: false
    onTriggered: {
      if (!root.busy) return
      if (root.stallStage === 0) {
        root.stallStage = 1
        root.waiting = true
        root.stateChanged()
        root.stallWatchdog.restart()
        return
      }
      root.stallStage = 2
      root.waiting = false
      root.stateChanged()
      root.forcedReason = "The assistant stopped responding, so it was restarted. Try that again."
      root.abort()
    }
  }

  // The abort did not land either, so the child is the problem, not the
  // question: replace it. handleExit reports the forced reason.
  property Timer abortEscalation: Timer {
    interval: 8000
    repeat: false
    onTriggered: {
      if (!root.busy) return
      root.abandon("The assistant stopped responding, so it was restarted. Try that again.")
    }
  }

  property Timer killTimer: Timer {
    interval: 3000
    repeat: false
    onTriggered: {
      if (!proc.running) return
      if (Number(proc.processId) !== root.killPid) return
      proc.signal(9)
    }
  }

  // No line after start means either the handshake is wedged or the child never
  // launched at all. Armed from start(), not onStarted, because a command that
  // cannot execute may never report `started` (Qt's FailedToStart has no
  // finished signal); `setpriv` normally exits 127 first, which the exit path
  // already turns into a plain sentence.
  property Timer startWatchdog: Timer {
    interval: 10000
    repeat: false
    onTriggered: {
      if (root.live) return
      if (!root.proc.running) {
        root.pendingRestart = false
        root.killPid = 0
        var detail = "pi could not be started. Check the command, the workspace and the model in "
          + "~/.config/omassistant/config.json."
        root.faultText = detail
        if (root.busy) {
          root.busy = false
          root.queuedPrompt = ""
          root.answered(false, detail, false)
        } else {
          root.died(detail)
        }
        root.stateChanged()
        return
      }
      // A slow first start is not a fault. Omarchy's managed mise may be
      // installing pi, and a restart here would interrupt that install. The
      // exit path (missing pi, a failed exec) and the handshake still decide;
      // the card says "connecting" in the meantime.
      return
    }
  }
}
