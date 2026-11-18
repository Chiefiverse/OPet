import QtQuick
import Quickshell
import Quickshell.Io

// Owns the pet's process: the bar widget only reads the properties below.
// Nothing here polls - state moves on Process signals and on the pet's own
// readiness line.
Item {
  id: root

  property var shell: null
  property var manifest: null

  readonly property var defaults: (manifest && manifest.barWidget && manifest.barWidget.defaults)
    ? manifest.barWidget.defaults : ({})

  // shell.json's per-widget entry is only visible to the bar widget, so the
  // widget pushes it in through applyConfig(); until then the manifest
  // defaults are the whole config.
  // Deliberately NOT bound: `manifest` is injected by the host and is
  // re-assigned whenever shell.json changes, so a bound cfg would reset to the
  // empty manifest defaults on every settings write - which is exactly the
  // moment the widget pushes its values in. The keys that came from the widget
  // are remembered separately so the defaults can never clobber them.
  property var cfg: ({
    binary: "",
    petManifest: "",
    petsDir: "",
    accessoriesDir: "",
    db: "",
    panelTop: 0,
    autoStart: false
  })
  property var overridden: ({})

  function fillDefaults() {
    var d = root.defaults
    if (!d) return
    var keys = ["binary", "petManifest", "petsDir", "accessoriesDir", "db", "panelTop", "autoStart"]
    for (var i = 0; i < keys.length; i++) {
      var k = keys[i]
      if (root.overridden[k]) continue
      if (d[k] === undefined || d[k] === null) continue
      root.cfg[k] = coerceConfig(k, d[k])
    }
  }

  function coerceConfig(k, v) {
    if (k === "panelTop") return Number(v) || 0
    if (k === "autoStart") return truthy(v)
    return String(v)
  }

  Component.onCompleted: fillDefaults()

  property bool desiredRunning: false
  property string phase: "stopped"
  property string reason: ""
  property int petPid: 0
  property int lastExit: -1
  property int failCount: 0
  property int resolvedPanelTop: 0
  property string panelTopSource: "unset"
  property bool restartPending: false
  property bool configApplied: false
  property var lastStatus: null
  property string statusError: ""
  property var tail: []

  property bool ctlBusy: false
  property string ctlMethod: ""
  property var ctlPending: null
  property bool probeHandled: false

  function truthy(v) {
    return v === true || String(v).toLowerCase() === "yes" || String(v) === "1"
  }

  function note(line) {
    var next = root.tail.slice(-4)
    next.push(String(line))
    root.tail = next
  }

  function stateJson() {
    return JSON.stringify({
      phase: root.phase,
      desired: root.desiredRunning,
      pid: root.petPid,
      fails: root.failCount,
      lastExit: root.lastExit,
      panelTop: root.resolvedPanelTop,
      panelTopSource: root.panelTopSource,
      restartPending: root.restartPending,
      reason: root.reason,
      binary: root.cfg.binary,
      status: root.lastStatus,
      statusError: root.statusError,
      tail: root.tail
    })
  }

  function applyConfig(overridesJson) {
    var o = null
    try {
      o = JSON.parse(overridesJson || "{}")
    } catch (e) {
      return "bad json: " + e
    }
    if (!o) return "ok"
    var changed = false
    for (var k in o) {
      if (root.cfg[k] === undefined) continue
      var v = o[k]
      if (v === null || v === undefined) continue
      v = coerceConfig(k, v)
      if (root.cfg[k] !== v) {
        root.overridden[k] = true
        root.cfg[k] = v
        if (k !== "autoStart") changed = true
      }
    }
    root.restartPending = changed && root.phase !== "stopped" && root.phase !== "refused"
    if (!root.configApplied) {
      root.configApplied = true
      if (root.cfg.autoStart && root.phase === "stopped") root.start()
    }
    return "ok"
  }

  function start() {
    root.desiredRunning = true
    root.failCount = 0
    if (root.phase === "running" || root.phase === "starting" || root.phase === "backoff")
      return "already " + root.phase
    root.beginLaunch()
    return "ok"
  }

  function stop() {
    root.desiredRunning = false
    retryTimer.stop()
    // A stop while the pre-flight is still in flight must also cancel the
    // launch it is about to trigger, not just the process that has not started.
    root.probeHandled = true
    if (root.phase === "running") return ctlSend("pet.stop", "", 200)
    if (root.phase === "starting" || root.phase === "backoff") {
      pet.running = false
      root.phase = "stopped"
      root.petPid = 0
      return "ok"
    }
    return "not running"
  }

  function pause() {
    if (root.phase !== "running") return "宠物没在跑"
    return ctlSend("pet.pause", "", 200)
  }

  function resume() {
    if (root.phase !== "running") return "宠物没在跑"
    return ctlSend("pet.resume", "", 200)
  }

  function pull(count) {
    if (root.phase !== "running") return "宠物没在跑"
    var n = Number(count)
    if (!isFinite(n) || n < 1) n = 1
    return ctlSend("pet.pull", JSON.stringify({ count: Math.floor(n) }), 3000)
  }

  function refreshStatus() {
    if (root.phase !== "running") return "宠物没在跑"
    return ctlSend("pet.status", "", 200)
  }

  // Pre-flight in one process: a missing binary only emits errorOccurred (never
  // exited), so the exit code cannot be the discovery mechanism, and the bar's
  // reserved height has to be read before the command line is fixed.
  function beginLaunch() {
    if (probe.running) return
    root.phase = "starting"
    root.reason = ""
    root.probeHandled = false
    probe.command = ["sh", "-c",
      'if [ ! -x "$1" ]; then echo NO_BIN; elif [ ! -f "$2" ]; then echo NO_MAN; '
      + 'else echo "OK $(hyprctl monitors -j 2>/dev/null | jq -r ".[0].reserved[1] // 0")"; fi',
      "opet-preflight", root.cfg.binary, root.cfg.petManifest]
    probe.running = true
  }

  function handleProbe(line) {
    if (root.probeHandled) return
    root.probeHandled = true
    var parts = String(line).trim().split(" ")
    if (parts[0] === "NO_BIN") {
      refuse("没找到可执行的 opet（当前 binary = \"" + root.cfg.binary + "\"）")
      return
    }
    if (parts[0] === "NO_MAN") {
      refuse("本体清单不存在（当前 petManifest = \"" + root.cfg.petManifest + "\"）")
      return
    }
    if (parts[0] !== "OK") {
      refuse("预检没有给出结果（读到 \"" + String(line).trim() + "\"）")
      return
    }
    var top = Number(root.cfg.panelTop)
    if (top > 0) {
      launch(top, "setting")
      return
    }
    var measured = Number(parts[1])
    if (isFinite(measured) && measured > 0) launch(measured, "hyprctl")
    else launch(26, "fallback")
  }

  function refuse(why) {
    root.phase = "refused"
    root.reason = why
    root.petPid = 0
  }

  function launch(top, source) {
    root.resolvedPanelTop = top
    root.panelTopSource = source
    var argv = [root.cfg.binary, "overlay", "--manifest", root.cfg.petManifest,
      "--ipc", "--seconds", "0"]
    if (top > 0) argv.push("--panel-top", String(top))
    if (root.cfg.db !== "") argv.push("--db", root.cfg.db)
    pet.command = argv
    var env = {}
    if (root.cfg.petsDir !== "") env.OPET_PETS = root.cfg.petsDir
    if (root.cfg.accessoriesDir !== "") env.OPET_ACCESSORIES = root.cfg.accessoriesDir
    pet.environment = env
    pet.running = true
  }

  function handleExit(code) {
    root.lastExit = code
    root.petPid = 0
    if (root.phase === "refused") return
    if (code === 2) {
      root.phase = "external"
      root.reason = "控制套接字被另一个活的实例占着（退出码 2）⇒ 这一轮不接管"
      return
    }
    if (!root.desiredRunning) {
      root.phase = "stopped"
      return
    }
    root.failCount += 1
    if (root.failCount >= 3) {
      refuse("连续 " + root.failCount + " 次异常退出（最后一次退出码 " + code + "）⇒ 停止重启。尾部日志：" + root.tail.join(" | "))
      return
    }
    root.phase = "backoff"
    root.reason = "第 " + root.failCount + " 次重启（退出码 " + code + "）"
    retryTimer.interval = 500 * Math.pow(2, root.failCount - 1)
    retryTimer.restart()
  }

  function ctlSend(method, paramsJson, timeoutMs) {
    if (root.cfg.binary === "") return "未配置 opet 可执行文件"
    var argv = [root.cfg.binary, "ctl"]
    if (timeoutMs) argv.push("--timeout", String(timeoutMs))
    argv.push(String(method))
    if (paramsJson) argv.push(paramsJson)
    if (root.ctlBusy) {
      root.ctlPending = { method: method, argv: argv }
      return "queued"
    }
    runCtl(method, argv)
    return "ok"
  }

  function runCtl(method, argv) {
    root.ctlBusy = true
    root.ctlMethod = String(method)
    ctlProc.command = argv
    ctlProc.running = true
  }

  function handleCtl(out) {
    var text = String(out || "").trim()
    var doc = null
    if (text !== "") {
      try {
        doc = JSON.parse(text)
      } catch (e) {
        doc = null
      }
    }
    var m = root.ctlMethod
    if (m === "pet.status") {
      if (doc && doc.ok === true) {
        root.lastStatus = doc
        root.statusError = ""
      } else {
        root.statusError = doc && doc.error
          ? String(doc.error.message || doc.error.code)
          : "pet.status 没有回话（宠物可能没在跑）"
      }
    } else if (m === "pet.stop" && !(doc && doc.ok === true)) {
      pet.running = false
    } else if (m === "pet.pull" || m === "pet.pause" || m === "pet.resume") {
      if (doc && doc.ok === true) root.refreshStatus()
    }
  }

  Timer {
    id: retryTimer
    repeat: false
    onTriggered: root.beginLaunch()
  }

  Process {
    id: probe
    stdout: SplitParser { splitMarker: "\n"; onRead: function(line) { root.handleProbe(line) } }
    onExited: function(code, status) {
      if (!root.probeHandled) root.refuse("预检进程退出码 " + code + "，且没有给出结论")
    }
  }

  Process {
    id: pet
    onStarted: root.petPid = Number(pet.processId) || 0
    stdout: SplitParser { splitMarker: "\n"; onRead: function(line) { root.note(line) } }
    // The socket line is the only readiness proof: "started" means the fork
    // worked, not that the atlas decoded or the surface mapped.
    stderr: SplitParser {
      splitMarker: "\n"
      onRead: function(line) {
        root.note(line)
        if (String(line).indexOf("ipc: ") === 0 && root.phase !== "running") {
          root.phase = "running"
          root.failCount = 0
          root.restartPending = false
          root.reason = ""
          root.petPid = Number(pet.processId) || 0
          root.refreshStatus()
        }
      }
    }
    onExited: function(code, status) { root.handleExit(code) }
  }

  Process {
    id: ctlProc
    stdout: SplitParser { splitMarker: "\n"; onRead: function(line) { root.handleCtl(line) } }
    onExited: function(code, status) {
      root.ctlBusy = false
      var p = root.ctlPending
      if (p) {
        root.ctlPending = null
        root.runCtl(p.method, p.argv)
      }
    }
  }

  IpcHandler {
    target: "opet.pet"

    function start(): string { return root.start() }
    function stop(): string { return root.stop() }
    function pause(): string { return root.pause() }
    function resume(): string { return root.resume() }
    function pull(count: string): string { return root.pull(count) }
    function refresh(): string { return root.refreshStatus() }
    function status(): string { return JSON.stringify(root.lastStatus || {}) }
    function state(): string { return root.stateJson() }
    function configure(overridesJson: string): string { return root.applyConfig(overridesJson) }
  }
}
