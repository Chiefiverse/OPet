import QtQuick
import Quickshell
import qs.Ui
import qs.Commons
import "i18n.js" as I18N

BarWidget {
  id: root

  moduleName: "opet.pet"

  readonly property var svc: bar && bar.shell ? bar.shell.serviceFor("opet.pet") : null
  readonly property string phase: svc && svc.phase ? String(svc.phase) : "absent"
  readonly property bool alive: phase === "running" || phase === "starting" || phase === "backoff"
  readonly property bool blocked: phase === "refused" || phase === "external" || phase === "absent"
  readonly property var status: svc ? svc.lastStatus : null

  property bool popupOpen: false

  // "language" is for the widget itself; the service gets its copy through
  // the same stamp push below, so both sides always render the same tongue.
  readonly property string lang: I18N.norm(
    root.settings && root.settings.language ? String(root.settings.language) : "en")

  implicitWidth: glyphBox.implicitWidth + Style.space(8)
  implicitHeight: barSize

  // The host only shows a tooltip for a target that reports its own hover
  // state (Bar.qml targetTooltipHovered), and the bare BarWidget base has no
  // such property — without this line showTooltip() is dropped silently.
  readonly property bool tooltipHovered: visible && hoverArea.containsMouse

  // The per-widget entry in shell.json is only readable here, so the effective
  // config is pushed into the service rather than duplicated by it.
  readonly property var overrideKeys: ["binary", "petManifest", "petsDir", "accessoriesDir", "db", "panelTop", "autoStart", "language"]

  // The host patches inline settings by reassigning `settings` on the live
  // widget, so the binding has to read that property itself: reading it only
  // inside setting() never re-fires and the service keeps the stale value.
  function stamp(source) {
    var out = {}
    for (var i = 0; i < root.overrideKeys.length; i++) {
      var k = root.overrideKeys[i]
      var v = source ? source[k] : undefined
      if (v !== undefined && v !== null) out[k] = v
    }
    return JSON.stringify(out)
  }

  property string overridesStamp: stamp(root.settings)

  function push() {
    // applyConfig is the service's own function; the identically named
    // IpcHandler method is not reachable through the service object.
    if (root.svc) root.svc.applyConfig(root.overridesStamp)
  }

  onOverridesStampChanged: push()
  onSvcChanged: push()
  Component.onCompleted: push()

  function tip() {
    var L = root.lang
    if (phase === "absent") return I18N.t(L, "tip_absent")
    if (phase === "refused") return I18N.t(L, "tip_refused", { reason: svc ? svc.reason : "" })
    if (phase === "external") return I18N.t(L, "tip_external")
    if (phase === "starting") return I18N.t(L, "tip_starting")
    if (phase === "backoff") return I18N.t(L, "tip_backoff", { reason: svc ? svc.reason : "" })
    if (phase === "running") return I18N.t(L, "tip_running", { pid: svc ? svc.petPid : 0 })
    return I18N.t(L, "tip_stopped")
  }

  Item {
    id: glyphBox
    anchors.centerIn: parent
    implicitWidth: glyph.width
    implicitHeight: glyph.height

    Image {
      id: glyph
      height: root.barSize
      width: height
      source: root.alive ? "assets/run.png" : "assets/logo.png"
      sourceSize.width: 48
      sourceSize.height: 48
      fillMode: Image.PreserveAspectFit
      smooth: true
    }
  }

  MouseArea {
    id: hoverArea
    anchors.fill: parent
    hoverEnabled: true
    cursorShape: Qt.PointingHandCursor
    acceptedButtons: Qt.LeftButton
    onClicked: root.popupOpen = !root.popupOpen
    onEntered: if (root.bar) root.bar.showTooltip(root, root.tip())
    onExited: if (root.bar) root.bar.hideTooltip(root)
  }

  PopupCard {
    id: popup
    anchorItem: root
    bar: root.bar
    owner: root
    open: root.popupOpen
    contentWidth: popup.fittedContentWidth(Style.space(280))
    contentHeight: popup.fittedContentHeight(column.implicitHeight)

    Column {
      id: column
      anchors.fill: parent
      spacing: Style.space(8)

      Text {
        textFormat: Text.PlainText
        text: root.tip()
        color: root.bar.barForeground
        font.family: root.bar.fontFamily
        font.pixelSize: Style.font.bodySmall
        width: parent.width
        wrapMode: Text.WordWrap
      }

      Row {
        spacing: Style.space(6)

        Button {
          text: root.alive ? I18N.t(root.lang, "btn_stop") : I18N.t(root.lang, "btn_start")
          bordered: true
          foreground: root.bar.foreground
          enabled: root.svc !== null
          onClicked: {
            if (!root.svc) return
            if (root.alive) root.svc.stop()
            else root.svc.start()
          }
        }

        Button {
          text: I18N.t(root.lang, "btn_pause")
          bordered: true
          foreground: root.bar.foreground
          enabled: root.phase === "running"
          onClicked: if (root.svc) root.svc.pause()
        }

        Button {
          text: I18N.t(root.lang, "btn_resume")
          bordered: true
          foreground: root.bar.foreground
          enabled: root.phase === "running"
          onClicked: if (root.svc) root.svc.resume()
        }
      }

      Row {
        spacing: Style.space(6)

        Button {
          text: I18N.t(root.lang, "btn_pull1")
          bordered: true
          foreground: root.bar.foreground
          enabled: root.phase === "running"
          onClicked: if (root.svc) root.svc.pull("1")
        }

        Button {
          text: I18N.t(root.lang, "btn_pull10")
          bordered: true
          foreground: root.bar.foreground
          enabled: root.phase === "running"
          onClicked: if (root.svc) root.svc.pull("10")
        }
      }

      Text {
        textFormat: Text.PlainText
        text: root.detail()
        color: Qt.darker(root.bar.barForeground, 1.4)
        font.family: root.bar.fontFamily
        font.pixelSize: Style.font.caption
        width: parent.width
        wrapMode: Text.WordWrap
      }
    }
  }

  function detail() {
    var L = root.lang
    if (!root.svc) return I18N.t(L, "detail_no_service")
    var bits = []
    var st = root.status
    if (st) {
      if (st.body) bits.push(I18N.t(L, "detail_body", { v: st.body }))
      if (st.lang) bits.push(I18N.t(L, "detail_lang", { v: st.lang }))
      bits.push(st.db
        ? I18N.t(L, "detail_db", { v: st.db })
        : I18N.t(L, "detail_demo"))
    }
    if (root.svc.resolvedPanelTop > 0)
      bits.push(I18N.t(L, "detail_panel",
        { v: root.svc.resolvedPanelTop, s: root.svc.panelTopSource }))
    if (root.svc.restartPending) bits.push(I18N.t(L, "detail_restart_pending"))
    if (root.svc.statusError) bits.push(String(root.svc.statusError))
    return bits.join(" · ")
  }
}
