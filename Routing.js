.pragma library

// Pure model routing: which configured model should answer this request.
//
// Nothing here touches Qt or pi. The host owns the catalog, the settings and the
// RPC switch; this file only decides, so the same code can be tested with node
// (test/routing.test.js). A wrong decision is a cost or a refusal, never an
// action: the permission broker, not the model, guards the desktop.
//
// The rules are deliberately boring and explainable:
//   * no automatic routing unless the person asked for it;
//   * one model means no switching at all;
//   * local-only never falls back to the cloud;
//   * short/simple work goes to Fast, longer or tool-shaped work to Best;
//   * a follow-up keeps the previous tier instead of flip-flopping;
//   * an image only goes to a model that declares image input.

var THINKING_LEVELS = ["off", "minimal", "low", "medium", "high"]

function text(value) {
  return value === undefined || value === null ? "" : String(value)
}

function modelKey(model) {
  if (!model) return ""
  return text(model.provider) + "/" + text(model.id)
}

function sameModel(a, b) {
  if (!a || !b) return false
  return text(a.provider) === text(b.provider) && text(a.id) === text(b.id)
}

function hasVision(model) {
  if (!model || !Array.isArray(model.input)) return false
  return model.input.indexOf("image") >= 0
}

function isLocal(model) {
  if (!model) return false
  if (text(model.provider).indexOf("omassistant-") === 0) return true
  var url = text(model.baseUrl).toLowerCase()
  var match = /^[a-z][a-z0-9+.-]*:\/\/([^/]+)/.exec(url)
  if (!match) return false
  var host = match[1]
  host = host.replace(/^\[([^\]]+)\].*$/, "$1")
  host = host.replace(/:\d+$/, "")
  return host === "localhost" || host === "127.0.0.1" || host === "::1"
}

function isFree(model) {
  if (!model || !model.cost || typeof model.cost !== "object") return false
  return Number(model.cost.input) === 0 && Number(model.cost.output) === 0
}

function costScore(model) {
  if (!model || !model.cost || typeof model.cost !== "object") return 999
  var score = Number(model.cost.input) + Number(model.cost.output)
  return isFinite(score) ? score : 999
}

function displayName(model) {
  if (!model) return ""
  return text(model.name) || text(model.id)
}

function providerLabel(provider) {
  var p = text(provider)
  var labels = {
    "google": "Google AI",
    "openrouter": "OpenRouter",
    "xai": "xAI",
    "openai": "OpenAI",
    "anthropic": "Anthropic",
    "omassistant-ollama": "Ollama",
    "omassistant-lmstudio": "LM Studio",
    "omassistant-llamacpp": "llama.cpp"
  }
  if (labels[p]) return labels[p]
  if (!p.length) return ""
  return p.charAt(0).toUpperCase() + p.slice(1)
}

function tags(model) {
  var list = []
  if (isLocal(model)) list.push("LOCAL")
  if (isFree(model)) list.push("FREE")
  if (hasVision(model)) list.push("VISION")
  return list
}

function thinkingLevel(spec, fallback) {
  var value = text(spec && spec.thinking)
  return value.length ? value : fallback
}

// A spec is {provider, id, thinking}. A missing provider matches by id alone,
// which keeps a config written before provider IDs existed usable.
function modelFor(spec, catalog) {
  if (!spec || !text(spec.id)) return null
  var id = text(spec.id)
  var provider = text(spec.provider)
  var found = null
  for (var i = 0; i < (catalog || []).length; i++) {
    var model = catalog[i]
    if (!model || text(model.id) !== id) continue
    if (!provider.length || text(model.provider) === provider) return model
    if (!found) found = model
  }
  return found
}

function firstLocal(catalog) {
  for (var i = 0; i < (catalog || []).length; i++) {
    if (isLocal(catalog[i])) return catalog[i]
  }
  return null
}

function cheapest(catalog, requireVision) {
  var best = null
  var bestScore = 1e9
  for (var i = 0; i < (catalog || []).length; i++) {
    var model = catalog[i]
    if (!model || !text(model.id) || !text(model.provider)) continue
    if (requireVision && !hasVision(model)) continue
    var score = (isFree(model) ? 0 : 1000) + costScore(model)
    if (score < bestScore
        || (score === bestScore && hasVision(model) && !hasVision(best))
        || (score === bestScore && hasVision(model) === hasVision(best)
            && Number(model.contextWindow || 0) > Number((best || {}).contextWindow || 0))) {
      best = model
      bestScore = score
    }
  }
  return best
}

function cheapestVision(catalog) {
  return cheapest(catalog, true)
}

// Resolve the configured tiers against what is actually available now. A tier
// whose model disappeared falls back to something sensible instead of failing
// the question.
function resolveAll(config, catalog, current) {
  config = config || {}
  catalog = catalog || []
  var defaultModel = modelFor(config.default, catalog)
    || modelFor({ provider: current && current.provider, id: current && current.id }, catalog)
    || (catalog.length ? catalog[0] : null)
  var best = modelFor(config.best, catalog) || defaultModel
  var local = modelFor(config.local, catalog)
  // A hand-edited config must not turn local-only into a cloud call.
  if (local && !isLocal(local)) local = null
  if (!local) local = firstLocal(catalog)
  // Fast falls back to the local model (explicitly discovered or chosen), then
  // to the default. Never to the cheapest cloud model: a listed price of zero
  // does not mean the provider will serve it (depleted credits, rate limits),
  // and routing a question to a model that cannot answer is worse than not
  // switching at all.
  var fast = modelFor(config.fast, catalog) || local || best
  return {
    default: defaultModel,
    fast: fast,
    best: best,
    local: local,
    defaultThinking: thinkingLevel(config.default, "low"),
    fastThinking: thinkingLevel(config.fast, "low"),
    bestThinking: thinkingLevel(config.best, thinkingLevel(config.default, "low")),
    localThinking: thinkingLevel(config.local, "low")
  }
}

function looksAction(question) {
  // Desktop and content work: a form, a deck, a spreadsheet or a document is
  // exactly the long multi-step work Best exists for. Without these words a
  // "fill out this form" request used to be classified as a quick question and
  // sent to Fast; the Best model was never reached even when it was configured.
  return /\b(open|launch|click|press|type|paste|copy|screenshot|screen|window|email|browser|terminal|volume|brightness|workspace|lock|play|pause|fill|form|field|submit|slide|slides|deck|presentation|spreadsheet|sheet|table|document|cell|row|column|format|rename|create|make|build|edit|update|add|remove|delete|move|select|choose|story|essay|article|poem|letter)\b/i
    .test(text(question))
}

function looksComplex(question) {
  var q = text(question)
  if (q.length > 240) return true
  if (q.indexOf("```") >= 0) return true
  return /\b(analys[ei]|analyse|analyze|compare|explain|why|design|plan|debug|refactor|review|research|summari[sz]e|translate|draft|trade-?offs?|architecture|step by step|in depth|in detail|pros and cons|form|presentation|slides?|deck|spreadsheet|document|table)\b/i
    .test(q)
}

function wantsDeep(question) {
  return /\b(in depth|in detail|thorough|thoroughly|carefully|think hard|take your time|detailed|comprehensive)\b/i
    .test(text(question))
}

function wantsQuick(question) {
  return /\b(quickly|briefly|one line|one sentence|short answer|quick question|tl;?dr)\b/i
    .test(text(question))
}

function isFollowUp(question) {
  var q = text(question).trim()
  if (q.length > 110) return false
  return /^(why|how|and|then|what about|continue|go on|more|really|ok|okay|yes|no|but|so)\b/i.test(q)
    || (q.length < 55 && /^(why|how)\?*$/i.test(q))
}

function block(message) {
  return { action: "block", message: message }
}

// May a prompt go out on the model that is live right now?
//
// Choosing a route is one decision; arriving at it is another. A switch can fail
// or time out, and afterwards the current model is whatever pi still had — which,
// with "keep everything on this computer" on, may be a cloud model. The fallback
// "use the current model" is therefore only allowed when that model is local.
//
// Returns "" when the prompt may go, or the sentence to show instead of sending.
// `liveModel` is the catalog entry for the model that is actually in use (null or
// undefined when it is not known yet — an unknown model is not a local one).
function liveSendBlocked(localOnly, liveModel) {
  if (!localOnly) return ""
  if (!liveModel || !text(liveModel.id)) {
    return "Keep everything on this computer is on, but no local model is active, so nothing was sent."
  }
  if (!isLocal(liveModel)) {
    return "Keep everything on this computer is on, and " + displayName(liveModel)
      + " is not a local model, so nothing was sent. Switch back to a local model and try again."
  }
  return ""
}

function use(tier, model, thinking, reason) {
  return {
    action: "use",
    tier: tier,
    provider: text(model.provider),
    id: text(model.id),
    name: displayName(model),
    thinking: thinking,
    reason: reason
  }
}

// The decision. Returns one of:
//   { action: "use", tier, provider, id, name, thinking, reason }
//   { action: "block", message }   — local-only/vision cannot be honoured
//   { action: "none" }             — leave the current model alone
function choose(question, image, contextChars, state) {
  state = state || {}
  var catalog = state.catalog || []
  var current = state.current || {}
  var resolved = resolveAll(state, catalog, current)

  if (state.localOnly) {
    if (!resolved.local) {
      return block("Keep everything on this computer is on, but no local model is set up. Open settings and add one.")
    }
    if (image && !hasVision(resolved.local)) {
      return block("The local model cannot look at screens. Choose a vision-capable local model, or turn off “Keep everything on this computer”.")
    }
    return use("local", resolved.local, resolved.localThinking, "on this computer")
  }

  if (!state.auto) {
    if (!resolved.default) return { action: "none" }
    if (image && !hasVision(resolved.default)) {
      return block("The model in use cannot see images. Pick a vision model in settings, or turn on automatic choice.")
    }
    return use("default", resolved.default, resolved.defaultThinking, "your default")
  }

  // One model means there is nothing to choose, but it still has to be able to
  // see the image before the question goes out.
  if (catalog.length < 2) {
    if (!resolved.default) return { action: "none" }
    if (image && !hasVision(resolved.default)) {
      return block("The only configured model cannot see images. Add a vision model in settings.")
    }
    return use("default", resolved.default, resolved.defaultThinking, "the only model")
  }

  var action = looksAction(question)
  var deep = wantsDeep(question)
  var quick = wantsQuick(question) && text(question).length < 300
  var complex = action || deep
    || (!quick && (looksComplex(question) || Number(contextChars || 0) > 18000))
  var follow = isFollowUp(question)
  var tier = complex ? "best" : "fast"
  if (follow && !deep && !quick && (state.lastTier === "fast" || state.lastTier === "best")) {
    tier = state.lastTier
  }

  var model = tier === "best" ? resolved.best : resolved.fast
  var thinking = tier === "best" ? resolved.bestThinking : resolved.fastThinking

  if (image) {
    if (!model || !hasVision(model)) {
      if (resolved.best && hasVision(resolved.best)) {
        tier = "best"
        model = resolved.best
        thinking = resolved.bestThinking
      } else {
        model = cheapestVision(catalog)
        if (!model) return block("No configured model can see images. Pick a vision model in settings.")
        tier = "vision"
        thinking = "low"
      }
    }
  }

  if (!model) {
    if (tier === "best" && resolved.fast) {
      model = resolved.fast
      thinking = resolved.fastThinking
      tier = "fast"
    } else if (resolved.best) {
      model = resolved.best
      thinking = resolved.bestThinking
      tier = "best"
    }
  }
  // A long conversation has already been paid for on the model that has it.
  // Switching down to Fast would re-send the whole context to a new model and
  // usually cost more than it saves; keep the current model unless the request
  // is actually complex (handled above) or the current model is already Fast.
  var currentModel = modelFor({ provider: current.provider, id: current.id }, catalog)
  if (currentModel && Number(contextChars || 0) > 6000 && tier === "fast"
      && !sameModel(currentModel, resolved.fast)) {
    return use("best", currentModel, text(current.thinking) || resolved.bestThinking,
      "long conversation")
  }

  if (!model) return { action: "none" }

  var reason = "quick question"
  if (tier === "best") reason = action ? "desktop action" : "longer request"
  if (image) reason = "looks at the screen"
  if (follow) reason = "follow-up"
  return use(tier, model, thinking, reason)
}
