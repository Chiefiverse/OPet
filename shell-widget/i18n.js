.pragma library

// Every user-visible string of the plugin, in English and Chinese.
// Service.qml composes its reasons through this file too, because the bar
// widget shows those reasons verbatim in its tooltip.

function norm(lang) {
  return lang === "zh" ? "zh" : "en"
}

var DICT = {
  // bar tooltip lines
  tip_absent: {
    en: "OPet: service not loaded (plugin disabled or not installed)",
    zh: "OPet：服务没加载（插件未启用或没装好）"
  },
  tip_refused: {
    en: "OPet refused to start: {reason}",
    zh: "OPet 拒绝启动：{reason}"
  },
  tip_external: {
    en: "OPet: another instance already owns the control socket; this round did not take over",
    zh: "OPet：已有一个实例在控制那个套接字，这一轮没接管"
  },
  tip_starting: {
    en: "OPet: starting",
    zh: "OPet：正在起"
  },
  tip_backoff: {
    en: "OPet: process died, restarting ({reason})",
    zh: "OPet：进程掉了，正在重启（{reason}）"
  },
  tip_running: {
    en: "OPet: running (pid {pid})",
    zh: "OPet：在跑（pid {pid}）"
  },
  tip_stopped: {
    en: "OPet: not running",
    zh: "OPet：没在跑"
  },

  // popup card buttons
  btn_stop: { en: "Stop", zh: "停止" },
  btn_start: { en: "Start", zh: "启动" },
  btn_pause: { en: "Pause", zh: "暂停" },
  btn_resume: { en: "Resume", zh: "恢复" },
  btn_pull1: { en: "Pull ×1", zh: "抽一次" },
  btn_pull10: { en: "Pull ×10", zh: "十连" },

  // popup card detail line
  detail_no_service: { en: "Service not loaded", zh: "服务没加载" },
  detail_body: { en: "Body {v}", zh: "本体 {v}" },
  detail_lang: { en: "Lang {v}", zh: "语言 {v}" },
  detail_db: { en: "Save {v}", zh: "存档 {v}" },
  detail_demo: { en: "Demo mode (nothing written to disk)", zh: "演示态（不写盘）" },
  detail_panel: { en: "Bar height {v} ({s})", zh: "栏高 {v}（{s}）" },
  detail_restart_pending: {
    en: "Config changed; takes effect on next start",
    zh: "配置已改，下次启动才生效"
  },

  // service-side reasons shown inside tip_refused / tip_backoff
  svc_no_binary: {
    en: "no executable opet found (current binary = \"{v}\")",
    zh: "没找到可执行的 opet（当前 binary = \"{v}\"）"
  },
  svc_no_manifest: {
    en: "body manifest does not exist (current petManifest = \"{v}\")",
    zh: "本体清单不存在（当前 petManifest = \"{v}\"）"
  },
  svc_probe_no_result: {
    en: "pre-flight gave no verdict (read \"{v}\")",
    zh: "预检没有给出结果（读到 \"{v}\"）"
  },
  svc_probe_exit: {
    en: "pre-flight process exited with code {code} and no verdict",
    zh: "预检进程退出码 {code}，且没有给出结论"
  },
  svc_external: {
    en: "control socket held by another live instance (exit code 2) => not taking over this round",
    zh: "控制套接字被另一个活的实例占着（退出码 2）⇒ 这一轮不接管"
  },
  svc_fail_loop: {
    en: "{n} consecutive abnormal exits (last exit code {code}) => restarts stopped. Tail log: {tail}",
    zh: "连续 {n} 次异常退出（最后一次退出码 {code}）⇒ 停止重启。尾部日志：{tail}"
  },
  svc_restart_n: {
    en: "restart #{n} (exit code {code})",
    zh: "第 {n} 次重启（退出码 {code}）"
  },
  svc_not_running: { en: "pet is not running", zh: "宠物没在跑" },
  svc_binary_unconfigured: {
    en: "opet executable not configured",
    zh: "未配置 opet 可执行文件"
  },
  svc_status_silent: {
    en: "pet.status did not answer (the pet may not be running)",
    zh: "pet.status 没有回话（宠物可能没在跑）"
  }
}

function t(lang, key, subs) {
  var entry = DICT[key]
  if (!entry) return key
  var s = entry[norm(lang)]
  if (subs) {
    for (var k in subs) {
      s = s.split("{" + k + "}").join(String(subs[k]))
    }
  }
  return s
}
