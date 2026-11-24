//! M3 两窗尖峰（计划 M3 的验证步，**先于任何 UI 接线**）。
//!
//! 这份例子只回答注册表重构最便宜也最要命的一件事：**宠物面 + 第二面共存时，
//! 消息路由、命中判定、关闭语义是否各归各**。四个判卷点：
//!
//! 1. 窗口列表里同时出现 `OPet` 与 `OPet-menu` 两扇（`xdotool search` 或 Wine 的窗口枚举）；
//! 2. 点击落在哪一面的实心像素，退出时计数就落在哪一面（互不串扰）；
//! 3. 两面的透明像素点击穿透到桌面，**不会**被另一面接走；
//! 4. 12 s 时编程式 `close(Menu)` ⇒ 打印一行"进程仍在"，进程**不死**；20 s 自己退。
//!
//! 跑法（本机 Wine，前缀 `~/.opet-win32`）：
//!
//! ```sh
//! cargo build --release -p overlay-win32 --target x86_64-pc-windows-gnu --example two_windows
//! wine target/x86_64-pc-windows-gnu/release/examples/two_windows.exe
//! ```
//!
//! ⚠️ 本文件**不进打包**：`scripts/build_windows.sh` 只摊 `opet-overlay-win32.exe`，
//! examples 只在开发机与 Wine 里存在。它也不是测试 —— 判卷要人眼 + 手点，`#[ignore]`
//! 那一族都轮不到它。

use std::time::{Duration, Instant};

/// 与 `overlay_common::atlas::ALPHA_HIT_THRESHOLD` 同一条阈值（真命中链读的就是它）。
#[cfg(windows)]
use overlay_common::atlas::ALPHA_HIT_THRESHOLD as ALPHA_MIN;

#[cfg(windows)]
mod spike {
    use super::*;
    use overlay_win32::window::{Client, Events, SurfaceKind, Wake};
    use std::cell::Cell;

    fn idx(kind: SurfaceKind) -> usize {
        match kind {
            SurfaceKind::Pet => 0,
            SurfaceKind::Menu => 1,
            SurfaceKind::Card => 2,
            SurfaceKind::Panel => 3,
        }
    }

    /// 每面一组计数；`hit` 只借 `&self` ⇒ 计数格用 `Cell`（单线程，无同步需求）。
    struct Counter {
        bufs: [Vec<u8>; 2],
        hits_client: [Cell<u32>; 2],
        hits_transparent: [Cell<u32>; 2],
        press: [Cell<u32>; 2],
        release: [Cell<u32>; 2],
        motion: [Cell<u32>; 2],
        capture_lost: Cell<u32>,
        dpi_changed: [Cell<u32>; 2],
    }

    impl Counter {
        fn new(pet: Vec<u8>, menu: Vec<u8>) -> Self {
            Self {
                bufs: [pet, menu],
                hits_client: [const { Cell::new(0) }; 2],
                hits_transparent: [const { Cell::new(0) }; 2],
                press: [const { Cell::new(0) }; 2],
                release: [const { Cell::new(0) }; 2],
                motion: [const { Cell::new(0) }; 2],
                capture_lost: Cell::new(0),
                dpi_changed: [const { Cell::new(0) }; 2],
            }
        }

        /// 读**自己那面** buffer 的 alpha 过没过阈值 —— 与真客户端同一个判据形状。
        fn solid(&self, kind: SurfaceKind, x: i32, y: i32) -> bool {
            let (w, h) = match kind {
                SurfaceKind::Pet => (PET_WH.0, PET_WH.1),
                SurfaceKind::Menu => (MENU_WH.0, MENU_WH.1),
                _ => return false,
            };
            if x < 0 || y < 0 || x >= w || y >= h {
                return false;
            }
            let i = ((y as usize) * (w as usize) + (x as usize)) * 4;
            self.bufs[idx(kind)][i + 3] as u32 >= ALPHA_MIN
        }
    }

    impl Events for Counter {
        fn hit(&self, kind: SurfaceKind, x: i32, y: i32) -> bool {
            let i = idx(kind);
            if self.solid(kind, x, y) {
                self.hits_client[i].set(self.hits_client[i].get() + 1);
                true
            } else {
                self.hits_transparent[i].set(self.hits_transparent[i].get() + 1);
                false
            }
        }

        fn press(&mut self, kind: SurfaceKind, _x: i32, _y: i32) {
            let i = idx(kind);
            self.press[i].set(self.press[i].get() + 1);
        }

        fn release(&mut self, kind: SurfaceKind, _x: i32, _y: i32) {
            let i = idx(kind);
            self.release[i].set(self.release[i].get() + 1);
        }

        fn motion(&mut self, kind: SurfaceKind, _x: i32, _y: i32) {
            let i = idx(kind);
            self.motion[i].set(self.motion[i].get() + 1);
        }

        fn capture_lost(&mut self, _kind: SurfaceKind) {
            self.capture_lost.set(self.capture_lost.get() + 1);
        }

        fn dpi_changed(&mut self, kind: SurfaceKind, _scale: f32) {
            let i = idx(kind);
            self.dpi_changed[i].set(self.dpi_changed[i].get() + 1);
        }
    }

    const PET_WH: (i32, i32) = (220, 220);
    const PET_AT: (i32, i32) = (200, 200);
    /// 本体面实心块：四边各留 40 px 全透边（透明区 = 判卷点 3 的靶）。
    const PET_CORE: (i32, i32, i32, i32) = (40, 40, 180, 180);
    const MENU_WH: (i32, i32) = (180, 120);
    const MENU_AT: (i32, i32) = (330, 260);
    /// 故意与本体面**重叠**的一块：叠放次序 + 命中分流一起判卷。
    const MENU_CORE: (i32, i32, i32, i32) = (12, 12, 168, 108);

    /// 预乘 BGRA8 的一块实心矩形，其余全透（0,0,0,0）。
    fn plate(w: i32, h: i32, core: (i32, i32, i32, i32), b: u8, g: u8, r: u8) -> Vec<u8> {
        let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
        for y in core.1..core.3 {
            for x in core.0..core.2 {
                let i = ((y as usize) * (w as usize) + (x as usize)) * 4;
                // alpha = 255 ⇒ 直通与预乘同值，写死 255 就不用在这份尖峰里重做预乘。
                buf[i] = b;
                buf[i + 1] = g;
                buf[i + 2] = r;
                buf[i + 3] = 255;
            }
        }
        buf
    }

    pub(super) fn run() -> Result<(), String> {
        let pet_buf = plate(PET_WH.0, PET_WH.1, PET_CORE, 255, 0, 255); // 品红
        let menu_buf = plate(MENU_WH.0, MENU_WH.1, MENU_CORE, 255, 64, 0); // 蓝
        let counter = Box::new(Counter::new(pet_buf.clone(), menu_buf.clone()));

        let mut client = Client::open(PET_AT, PET_WH)?;
        client.spawn(SurfaceKind::Menu, MENU_AT, MENU_WH)?;
        client.present(SurfaceKind::Pet, PET_AT, &pet_buf)?;
        client.present(SurfaceKind::Menu, MENU_AT, &menu_buf)?;
        println!(
            "spike: 两扇已上映 pet{:?}@{:?} + menu{:?}@{:?}（重叠区故意相交）；dpi_path={:?} timer={:?}",
            PET_WH,
            PET_AT,
            MENU_WH,
            MENU_AT,
            client.dpi_path(),
            client.timer_path()
        );
        println!("spike: 判卷 = 窗口列表应有 OPet 与 OPet-menu 两扇；分别点实心块/透明边/重叠区");

        // `pump` 要的 `'static mut` 借用只能由稳定堆地址形成（与 app.rs 的 drive 同一条纪律）。
        let raw = Box::into_raw(counter);
        let start = Instant::now();
        let close_at = Duration::from_secs(12);
        let quit_at = Duration::from_secs(20);
        let mut menu_closed = false;
        let outcome: Result<(), String> = loop {
            let t = start.elapsed();
            if !menu_closed && t >= close_at {
                match client.close(SurfaceKind::Menu) {
                    Ok(()) => {
                        println!("spike: t={t:?} close(Menu) 完成 ⇒ 进程仍在（退出旗未被落）")
                    }
                    Err(e) => break Err(format!("close(Menu) 失败: {e}")),
                }
                if client.quit() {
                    break Err(
                        "close(Menu) 之后退出旗竟然是真 ⇒ 关闭语义漏了（WM_DESTROY 把进程带走）"
                            .into(),
                    );
                }
                menu_closed = true;
            }
            if t >= quit_at || client.quit() {
                break Ok(());
            }
            let wait = (quit_at - t).as_millis().min(100) as u32;
            // SAFETY: 与 drive() 同形 —— pump 返回时借用已随 `Slot::ev` 清回 `None`，
            // 这一轮之后、下一轮形成之前没有任何存活引用；这块堆内存在循环外没有第二个所有者。
            let ev: &'static mut dyn Events = unsafe { &mut *raw };
            match client.pump(ev, wait) {
                Ok(Wake::Failed(code)) => {
                    if client.quit() {
                        break Ok(());
                    }
                    break Err(format!("等待失败且非退出（GetLastError={code:#X}）"));
                }
                Err(e) => break Err(e),
                Ok(_) => {}
            }
        };
        // SAFETY: 循环已终止，`Slot::ev` 是 `None`（pump 的收尾），此后无人再借这块。
        let counter = unsafe { Box::from_raw(raw) };
        println!(
            "spike: 计数 pet=[hit✓ {} / hit✗ {} press {} release {} motion {}] \
             menu=[hit✓ {} / hit✗ {} press {} release {} motion {}] \
             capture_lost={} dpi=[pet {} menu {}]",
            counter.hits_client[0].get(),
            counter.hits_transparent[0].get(),
            counter.press[0].get(),
            counter.release[0].get(),
            counter.motion[0].get(),
            counter.hits_client[1].get(),
            counter.hits_transparent[1].get(),
            counter.press[1].get(),
            counter.release[1].get(),
            counter.motion[1].get(),
            counter.capture_lost.get(),
            counter.dpi_changed[0].get(),
            counter.dpi_changed[1].get(),
        );
        outcome
    }
}

fn main() {
    #[cfg(windows)]
    {
        if let Err(e) = spike::run() {
            eprintln!("spike: 失败: {e}");
            std::process::exit(1);
        }
    }
    #[cfg(not(windows))]
    {
        // 宿主上 examples 也参与 `cargo test`/`clippy --all-targets` 的编译 ⇒ 这个分支
        // 存在的意义是"编译过"，不是"跑过"：`window` 模块是 `#[cfg(windows)]`。
        println!(
            "two_windows 尖峰只在 Windows/Wine 上有意义（window 层是 cfg(windows)）；宿主这趟 = 只编译"
        );
        let _ = (Duration::from_secs(1), Instant::now());
    }
}
