import QtQuick
import Quickshell
import qs.Ui
import qs.Commons

BarWidget {
  id: root

  moduleName: "opet.pet"

  readonly property var svc: bar && bar.shell ? bar.shell.serviceFor("opet.pet") : null
  readonly property string phase: svc && svc.phase ? String(svc.phase) : "absent"
  readonly property bool alive: phase === "running" || phase === "starting" || phase === "backoff"
  readonly property bool blocked: phase === "refused" || phase === "external" || phase === "absent"
  readonly property var status: svc ? svc.lastStatus : null

  property bool popupOpen: false

  implicitWidth: glyphBox.implicitWidth + Style.space(8)
  implicitHeight: barSize

  // The host only shows a tooltip for a target that reports its own hover
  // state (Bar.qml targetTooltipHovered), and the bare BarWidget base has no
  // such property — without this line showTooltip() is dropped silently.
  readonly property bool tooltipHovered: visible && hoverArea.containsMouse

  // The per-widget entry in shell.json is only readable here, so the effective
  // config is pushed into the service rather than duplicated by it.
  readonly property var overrideKeys: ["binary", "petManifest", "petsDir", "accessoriesDir", "db", "panelTop", "autoStart"]

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
    if (phase === "absent") return "OPet：服务没加载（插件未启用或没装好）"
    if (phase === "refused") return "OPet 拒绝启动：" + (svc ? svc.reason : "")
    if (phase === "external") return "OPet：已有一个实例在控制那个套接字，这一轮没接管"
    if (phase === "starting") return "OPet：正在起"
    if (phase === "backoff") return "OPet：进程掉了，正在重启（" + (svc ? svc.reason : "") + "）"
    if (phase === "running") return "OPet：在跑（pid " + (svc ? svc.petPid : 0) + "）"
    return "OPet：没在跑"
  }

  Item {
    id: glyphBox
    anchors.centerIn: parent
    implicitWidth: glyph.implicitWidth
    implicitHeight: glyph.implicitHeight

    Text {
      id: glyph
      textFormat: Text.PlainText
      text: root.alive ? "●" : (root.blocked ? "!" : "○")
      color: root.phase === "running" ? root.bar.barForeground
        : (root.blocked ? root.bar.urgent : Qt.darker(root.bar.barForeground, 1.6))
      font.family: root.bar.fontFamily
      font.pixelSize: Style.font.body
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
          text: root.alive ? "停止" : "启动"
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
          text: "暂停"
          bordered: true
          foreground: root.bar.foreground
          enabled: root.phase === "running"
          onClicked: if (root.svc) root.svc.pause()
        }

        Button {
          text: "恢复"
          bordered: true
          foreground: root.bar.foreground
          enabled: root.phase === "running"
          onClicked: if (root.svc) root.svc.resume()
        }
      }

      Row {
        spacing: Style.space(6)

        Button {
          text: "抽一次"
          bordered: true
          foreground: root.bar.foreground
          enabled: root.phase === "running"
          onClicked: if (root.svc) root.svc.pull("1")
        }

        Button {
          text: "十连"
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
    if (!root.svc) return "服务没加载"
    var bits = []
    var st = root.status
    if (st) {
      if (st.body) bits.push("本体 " + st.body)
      if (st.lang) bits.push("语言 " + st.lang)
      bits.push(st.db ? "存档 " + st.db : "演示态（不写盘）")
    }
    if (root.svc.resolvedPanelTop > 0)
      bits.push("栏高 " + root.svc.resolvedPanelTop + " (" + root.svc.panelTopSource + ")")
    if (root.svc.restartPending) bits.push("配置已改，下次启动才生效")
    if (root.svc.statusError) bits.push(String(root.svc.statusError))
    return bits.join(" · ")
  }
}
