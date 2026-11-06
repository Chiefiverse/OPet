//! **store-only** zip 的只读索引与最小写入口（设计书 §3.11.1「zip，无压缩或 deflate」里
//! 选**前半个**选项的那一条实现，依据 = §9.2 第 21 条的实测：同一份 lossless WebP，
//! `zip -9` 比 `zip -0` 只小 0.029% ⇒ 引一个压缩库换不来任何体积）。
//!
//! 三条刻意的取舍，改代码前先读：
//!
//! 1. **只认 method=0（stored）**。见到 deflate 就报一句能照着办的话（"这个包用了 deflate，
//!    请用 `zip -0 -r` 重打"），而不是悄悄支持一半的 zip 实现 —— 半套压缩支持最容易长出的
//!    形状是"我们的官方包能装、别人的不能装"，而那正是没人会报的 bug。
//! 2. **随机访问，不整块读进内存**。只 seek 到中央目录那一小段；取条目字节时按
//!    `(data_offset, len)` 读那一段。64 MB 的单包上限（§3.11.1）不该变成 64 MB 的 RSS。
//! 3. **`Vec<ZipEntry>` 按 name 升序**（[`ZipIndex::open`] 里排序）。`read_dir` 的顺序由文件系统
//!    给、zip 中央目录的顺序由打包者给，两者都不可依赖 ⇒ 不排序就是"同一台机器两次启动
//!    列表不同"（`app.rs` 的 `discover_accessories` 为了同一件事排过序，理由一字不差）。

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// 中央目录条目的固定头长度（签名往后到"本地头偏移"结束）。
const CENTRAL_FIXED: usize = 46;
/// 本地文件头的固定长度。真实数据的起点 = 本地头偏移 + 30 + name_len + extra_len。
const LOCAL_FIXED: usize = 30;
const SIG_CENTRAL: u32 = 0x0201_4b50;
const SIG_LOCAL: u32 = 0x0403_4b50;
const SIG_EOCD: u32 = 0x0605_4b50;
/// EOCD 里注释字段可变长 ⇒ 从文件尾往前最多扫这么多个字节找签名。
const EOCD_SEARCH: u64 = 22 + 65_535;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZipError {
    /// 读文件失败（不存在、没权限、目录当文件开…）。
    Io(String),
    /// 尾部找不到 EOCD ⇒ 这不是 zip，或被截断。
    NoEocd,
    /// 中央目录长度与 EOCD 声明的条目数对不上 ⇒ 结构坏了。
    TruncatedCentral,
    /// 条目名不是合法 UTF-8。
    BadNameEncoding,
    /// 条目名越界（绝对路径、`..`、空段）⇒ **拒绝，不是跳过**（§3.11.1 第三条）。
    UnsafeName { name: String, reason: &'static str },
    /// method != 0：这个包是压缩过的，本加载器不解压。
    Deflate { name: String },
    /// 条目声明自己是符号链接（外部属性里的 Unix mode）。
    Symlink { name: String },
}

impl ZipError {
    pub fn message(&self) -> String {
        match self {
            ZipError::Io(m) => format!("zip 读取失败: {m}"),
            ZipError::NoEocd => {
                "zip 尾部找不到结束标记（EOCD）：这不是 zip 文件，或被截断了".into()
            }
            ZipError::TruncatedCentral => "zip 中央目录比 EOCD 声明的短：文件被截断了".into(),
            ZipError::BadNameEncoding => "zip 里有条目名不是合法 UTF-8".into(),
            ZipError::UnsafeName { name, reason } => {
                format!("zip 条目 {name} 越界：{reason} ⇒ 整个包拒绝，一个字都不解")
            }
            ZipError::Deflate { name } => format!(
                "zip 条目 {name} 用了 deflate（method≠0）。本加载器只读 store；\
                 请用 `zip -0 -r pack.petpack manifest.toml pets accessories` 重打这个包"
            ),
            ZipError::Symlink { name } => {
                format!("zip 条目 {name} 是符号链接 ⇒ 拒绝（它能指向包外任何位置）")
            }
        }
    }
}

/// 一个包内条目。**偏移与长度都是"数据"的**，不是本地头的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipEntry {
    pub name: String,
    pub len: u64,
    pub crc32: u32,
    pub data_offset: u64,
    pub is_dir: bool,
}

/// 一个 `.petpack` 的中央目录索引。打开一次、随取随用，不驻留包体。
#[derive(Debug, Clone)]
pub struct ZipIndex {
    file: PathBuf,
    entries: Vec<ZipEntry>,
}

impl ZipIndex {
    /// 读中央目录并**校验全部条目名**。任何一条越界都让**整个包**出局：
    /// 跳过坏条目等于"装了一个我们不知道长什么样的包"，那比不装更糟。
    pub fn open(path: &Path) -> Result<Self, ZipError> {
        let mut f = std::fs::File::open(path)
            .map_err(|e| ZipError::Io(format!("{}: {e}", path.display())))?;
        let file_len = f.metadata().map_err(|e| ZipError::Io(e.to_string()))?.len();
        if file_len < 22 {
            return Err(ZipError::NoEocd);
        }
        let tail_len = file_len.min(EOCD_SEARCH);
        let mut tail = vec![0u8; tail_len as usize];
        // `SeekFrom::End(+n)` 是"尾后再 n 字节"（读到 EOF ⇒ read_exact 必然短读），
        // 要的是"往前 n 字节"。第一条单测就是这么炸出来的。
        f.seek(SeekFrom::End(-(tail_len as i64)))
            .map_err(|e| ZipError::Io(e.to_string()))?;
        f.read_exact(&mut tail)
            .map_err(|e| ZipError::Io(e.to_string()))?;
        let at = scan_eocd(&tail).ok_or(ZipError::NoEocd)?;
        let eocd = &tail[at..at + 22];
        let count = u16::from_le_bytes([eocd[10], eocd[11]]) as usize;
        let central_size = u32::from_le_bytes([eocd[12], eocd[13], eocd[14], eocd[15]]) as u64;
        let central_off = u32::from_le_bytes([eocd[16], eocd[17], eocd[18], eocd[19]]) as u64;
        if central_off + central_size > file_len {
            return Err(ZipError::TruncatedCentral);
        }

        let mut central = vec![0u8; central_size as usize];
        f.seek(SeekFrom::Start(central_off))
            .map_err(|e| ZipError::Io(e.to_string()))?;
        f.read_exact(&mut central)
            .map_err(|e| ZipError::Io(e.to_string()))?;

        let mut entries: Vec<ZipEntry> = Vec::with_capacity(count);
        let mut p = 0usize;
        let mut seen = 0usize;
        while p + CENTRAL_FIXED <= central.len() {
            if u32::from_le_bytes([central[p], central[p + 1], central[p + 2], central[p + 3]])
                != SIG_CENTRAL
            {
                break;
            }
            let method = u16::from_le_bytes([central[p + 10], central[p + 11]]);
            let crc = u32::from_le_bytes([
                central[p + 16],
                central[p + 17],
                central[p + 18],
                central[p + 19],
            ]);
            let comp_len = u32::from_le_bytes([
                central[p + 20],
                central[p + 21],
                central[p + 22],
                central[p + 23],
            ]) as u64;
            let uncomp_len = u32::from_le_bytes([
                central[p + 24],
                central[p + 25],
                central[p + 26],
                central[p + 27],
            ]) as u64;
            let name_len = u16::from_le_bytes([central[p + 28], central[p + 29]]) as usize;
            let extra_len = u16::from_le_bytes([central[p + 30], central[p + 31]]) as usize;
            let comment_len = u16::from_le_bytes([central[p + 32], central[p + 33]]) as usize;
            let ext_attr = u32::from_le_bytes([
                central[p + 38],
                central[p + 39],
                central[p + 40],
                central[p + 41],
            ]);
            let local_off = u32::from_le_bytes([
                central[p + 42],
                central[p + 43],
                central[p + 44],
                central[p + 45],
            ]) as u64;
            let name_end = p + CENTRAL_FIXED + name_len;
            if name_end > central.len() {
                return Err(ZipError::TruncatedCentral);
            }
            let name = String::from_utf8(central[p + CENTRAL_FIXED..name_end].to_vec())
                .map_err(|_| ZipError::BadNameEncoding)?;
            seen += 1;

            if (ext_attr >> 16) & 0xF000 == 0xA000 {
                return Err(ZipError::Symlink { name });
            }
            check_safe_name(&name)?;
            if method != 0 {
                return Err(ZipError::Deflate { name });
            }
            // stored ⇒ 压缩后长度必须等于原始长度。不等就是索引在说谎，读出来的字节会错位。
            if comp_len != uncomp_len {
                return Err(ZipError::TruncatedCentral);
            }
            let is_dir = name.ends_with('/');
            let data_offset = if is_dir {
                0
            } else {
                local_data_offset(&mut f, local_off).map_err(|e| ZipError::Io(e.to_string()))?
            };
            entries.push(ZipEntry {
                name,
                len: uncomp_len,
                crc32: crc,
                data_offset,
                is_dir,
            });
            p = name_end + extra_len + comment_len;
        }
        if seen != count {
            return Err(ZipError::TruncatedCentral);
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Self {
            file: path.to_path_buf(),
            entries,
        })
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    pub fn entries(&self) -> &[ZipEntry] {
        &self.entries
    }

    /// 前缀查询（`accessories/xmashat/`）。`entries()` 已排序 ⇒ 结果是确定性的。
    pub fn with_prefix(&self, prefix: &str) -> Vec<&ZipEntry> {
        self.entries
            .iter()
            .filter(|e| !e.is_dir && e.name.starts_with(prefix))
            .collect()
    }

    pub fn find(&self, name: &str) -> Option<&ZipEntry> {
        self.entries.iter().find(|e| e.name == name && !e.is_dir)
    }

    /// 取一个条目的字节并**验 CRC**。返回坏了的字节比不返回更糟：调用方会把它当资产解码。
    pub fn read(&self, name: &str) -> Result<Vec<u8>, ZipError> {
        let e = self
            .find(name)
            .ok_or_else(|| ZipError::Io(format!("包 {} 里没有条目 {name}", self.file.display())))?;
        self.read_entry(e)
    }

    pub fn read_entry(&self, e: &ZipEntry) -> Result<Vec<u8>, ZipError> {
        let mut f = std::fs::File::open(&self.file)
            .map_err(|err| ZipError::Io(format!("{}: {err}", self.file.display())))?;
        let mut buf = vec![0u8; e.len as usize];
        if e.len > 0 {
            f.seek(SeekFrom::Start(e.data_offset))
                .map_err(|err| ZipError::Io(err.to_string()))?;
            f.read_exact(&mut buf)
                .map_err(|err| ZipError::Io(err.to_string()))?;
        }
        let got = crc32(&buf);
        if got != e.crc32 {
            return Err(ZipError::Io(format!(
                "条目 {} 的 CRC 对不上（包内记 {:08x}，实算 {got:08x}）⇒ 这个包坏了",
                e.name, e.crc32
            )));
        }
        Ok(buf)
    }
}

/// 本地头里也有 name_len/extra_len，而且**可以和中央目录里的不一样** ⇒ 数据起点只能从本地头算。
fn local_data_offset(f: &mut std::fs::File, local_off: u64) -> std::io::Result<u64> {
    let mut h = [0u8; LOCAL_FIXED];
    f.seek(SeekFrom::Start(local_off))?;
    f.read_exact(&mut h)?;
    if u32::from_le_bytes([h[0], h[1], h[2], h[3]]) != SIG_LOCAL {
        return Err(std::io::Error::other("zip 本地文件头签名不对"));
    }
    let name_len = u16::from_le_bytes([h[26], h[27]]) as u64;
    let extra_len = u16::from_le_bytes([h[28], h[29]]) as u64;
    Ok(local_off + LOCAL_FIXED as u64 + name_len + extra_len)
}

fn scan_eocd(tail: &[u8]) -> Option<usize> {
    if tail.len() < 22 {
        return None;
    }
    let mut i = tail.len() - 22;
    loop {
        if u32::from_le_bytes([tail[i], tail[i + 1], tail[i + 2], tail[i + 3]]) == SIG_EOCD {
            // 注释长度必须刚好吃到尾部 ⇒ 挡掉文件内容里偶然出现的伪签名
            let clen = u16::from_le_bytes([tail[i + 20], tail[i + 21]]) as usize;
            if i + 22 + clen == tail.len() {
                return Some(i);
            }
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    }
}

/// §3.11.1 第三条 + §Security(:1200)：绝对路径、`..`、空段一律拒绝。
/// **反斜杠不当分隔符**（zip 规范用 `/`），所以 `a\..\b` 走的是"名字里就有个点dot段"这一条判定，
/// 不需要为 Windows 另开一套 —— 开那套反而会让同一个包在两个平台上得到两种判定。
///
/// `pub`：这条规则**只有一份**。[`super::source::AssetItem::member`] 拿它判"清单里写的
/// 成员名"，[`super::source::AssetDir::locate`] 拿它判"id"，两边各自写一遍就会漂移。
pub fn check_safe_name(name: &str) -> Result<(), ZipError> {
    let bad = |reason: &'static str| ZipError::UnsafeName {
        name: name.to_string(),
        reason,
    };
    if name.is_empty() {
        return Err(bad("空条目名"));
    }
    if name.starts_with('/') {
        return Err(bad("绝对路径"));
    }
    if name.contains(':') || name.contains(char::from_u32(0).unwrap()) {
        return Err(bad("名字里带盘符或 NUL"));
    }
    for seg in name.split('/') {
        if seg == ".." {
            return Err(bad("路径穿越（..）"));
        }
        if seg.is_empty() && !name.ends_with('/') {
            return Err(bad("空路径段（连续两个 /）"));
        }
    }
    Ok(())
}

/// IEEE CRC-32（zip 用的那个多项式 0xEDB88320）。逐字节查表，无浮点、无平台相关归约 ⇒ 确定性。
pub fn crc32(bytes: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        t
    });
    let mut c = 0xFFFF_FFFFu32;
    for &b in bytes {
        c = table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

/// **写一个 store-only zip**。它存在的理由**只有测试**：单测与集成测试要能自造夹具，
/// 不能依赖 CI 机器上有没有 `/usr/bin/zip`（那条依赖会让测试在有的机器上静默跳过 = 假门禁）。
/// 出包那条路不走这里，走 `zip -0 -r`（v1 没有 `opet pack make`，别再按这个名字找命令）。
///
/// 条目按传入顺序写；调用方自己保证排序（确定性立在校验侧，不在这里）。
pub fn write_store_zip(entries: &[(&str, &[u8])]) -> Result<Vec<u8>, ZipError> {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    let mut count = 0u16;
    for (name, bytes) in entries {
        check_safe_name(name)?;
        let local_off = out.len() as u32;
        let crc = crc32(bytes);
        let n = name.as_bytes();
        out.extend_from_slice(&SIG_LOCAL.to_le_bytes());
        out.extend_from_slice(&[20, 0]); // version needed
        out.extend_from_slice(&[0, 0]); // flags：不设 0x800，名字按 UTF-8 直存（读侧不依赖该位）
        out.extend_from_slice(&[0, 0]); // method = store
        out.extend_from_slice(&[0, 0]); // time
        out.extend_from_slice(&[0, 0]); // date
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&(n.len() as u16).to_le_bytes());
        out.extend_from_slice(&[0, 0]); // extra len
        out.extend_from_slice(n);
        out.extend_from_slice(bytes);

        central.extend_from_slice(&SIG_CENTRAL.to_le_bytes());
        central.extend_from_slice(&[20, 0]); // version made by
        central.extend_from_slice(&[20, 0]); // version needed
        central.extend_from_slice(&[0, 0]); // flags
        central.extend_from_slice(&[0, 0]); // method = store
        central.extend_from_slice(&[0, 0]); // time
        central.extend_from_slice(&[0, 0]); // date
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        central.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        central.extend_from_slice(&(n.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0, 0]); // extra
        central.extend_from_slice(&[0, 0]); // comment
        central.extend_from_slice(&[0, 0]); // disk start
        central.extend_from_slice(&[0, 0]); // internal attrs
        central.extend_from_slice(&0u32.to_le_bytes()); // external attrs：普通文件，**不是** symlink
        central.extend_from_slice(&local_off.to_le_bytes());
        central.extend_from_slice(n);
        count += 1;
    }
    let central_off = out.len() as u32;
    out.extend_from_slice(&central);
    let central_size = central.len() as u32;
    out.extend_from_slice(&SIG_EOCD.to_le_bytes());
    out.extend_from_slice(&[0, 0]); // disk
    out.extend_from_slice(&[0, 0]); // disk with central dir
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&central_size.to_le_bytes());
    out.extend_from_slice(&central_off.to_le_bytes());
    out.extend_from_slice(&[0, 0]); // comment len
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 夹具目录：每个测试一个，按测试名 + pid 命名 ⇒ 同一次运行里并行线程也不撞车。
    /// 开测先 `remove_dir_all` ⇒ 上一次失败的残留不会伪装成"这次的产物"。
    fn scratch(test: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("opet_zip_{test}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("临时目录建得起来");
        dir
    }

    fn put(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).expect("写临时文件不该失败");
        p
    }

    /// **等长**原地替换（条目名的长度决定后面每一个偏移，改长度就是把整个索引搅乱）。
    fn sub_all(hay: &mut [u8], from: &[u8], to: &[u8]) {
        assert_eq!(from.len(), to.len(), "夹具只在等长时成立");
        let mut i = 0;
        let mut hits = 0;
        while i + from.len() <= hay.len() {
            if &hay[i..i + from.len()] == from {
                hay[i..i + from.len()].copy_from_slice(to);
                i += from.len();
                hits += 1;
            } else {
                i += 1;
            }
        }
        assert!(
            hits >= 2,
            "条目名至少出现在本地头与中央目录各一次，实际命中 {hits}"
        );
    }

    /// 我们自己的写器不写 EOCD 注释 ⇒ 结束标记正好在末尾前 22 字节，从中取中央目录偏移。
    fn central_off(zip: &[u8]) -> usize {
        let e = zip.len() - 22;
        assert_eq!(
            u32::from_le_bytes([zip[e], zip[e + 1], zip[e + 2], zip[e + 3]]),
            SIG_EOCD,
            "夹具的 EOCD 位置假设不成立"
        );
        u32::from_le_bytes([zip[e + 16], zip[e + 17], zip[e + 18], zip[e + 19]]) as usize
    }

    /// 单条目夹具的中央记录起点（写器的 extra/comment 都是 0 ⇒ 记录 = 46 + 名字长）。
    fn first_record(zip: &[u8]) -> usize {
        central_off(zip)
    }

    /// 一份"名字可被等长换成三种坏名字"的单条目包。名字 12 字节。
    fn one_entry_zip() -> Vec<u8> {
        write_store_zip(&[("data/pwn.txt", b"payload!")]).expect("安全名字写得出来")
    }

    #[test]
    fn round_trips_through_our_own_writer() {
        let dir = scratch("round_trip");
        let bytes = write_store_zip(&[
            ("manifest.toml", b"id = \"x\"\n"),
            ("pets/demo/pet.json", b"{\"format\":2}"),
        ])
        .unwrap();
        let f = put(&dir, "a.zip", &bytes);
        let idx = ZipIndex::open(&f).expect("自己写的包自己必须读得动");
        assert_eq!(idx.entries().len(), 2);
        assert_eq!(idx.read("manifest.toml").unwrap(), b"id = \"x\"\n");
        assert_eq!(idx.read("pets/demo/pet.json").unwrap(), b"{\"format\":2}");
        assert_eq!(idx.file(), f.as_path());
        assert!(
            idx.read("没有这个条目").is_err(),
            "读不存在的条目要报错，不是给空字节"
        );
    }

    /// **CRC 的已知答案**（IEEE 802.3 的标准校验串）。
    /// round-trip 测不出这条：一个"自洽但写错"的多项式两端会一起错。它唯一的用处是
    /// 和别的实现对上数 —— 而对上数正是下面那条读真 `zip -0` 夹具的前提。
    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    /// 读**真 `zip -0 -r` 打的包**（Info-ZIP 3.0，本机产、进仓库当夹具）。
    /// 存在的理由只有一条：`write_store_zip` 与 `ZipIndex::open` 是同一份心智模型的两侧，
    /// 两边一起错就一起绿 ⇒ 一个外部生产者的包是唯一能证明"偏移算对了、CRC 对上了"的东西。
    /// 它同时钉住 §9.2 第 21 条那条打包配方（`zip -0 -r`）真的能被我方读侧接住。
    #[test]
    fn reads_a_real_stored_archive_from_another_implementation() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/zip_sample_stored.zip");
        let idx = ZipIndex::open(&fixture).expect("真 zip -0 必须读得动");
        // 7 条 = 3 个文件 + 4 个目录条目（`zip -r .` 会把中间目录也写进去）
        assert_eq!(idx.entries().len(), 7, "夹具条目数变了？");
        assert_eq!(idx.entries()[0].name, "accessories/");
        assert_eq!(
            idx.with_prefix("accessories/").len(),
            1,
            "目录条目不该进前缀结果"
        );
        assert_eq!(
            idx.with_prefix("accessories/hat/")[0].name.as_str(),
            "accessories/hat/accessory.json"
        );
        // 这两个 CRC 是 `unzip -v` 打出来的 ⇒ 我方算法与 Info-ZIP 对上数
        assert_eq!(idx.find("manifest.toml").unwrap().crc32, 0xe814_f876);
        assert_eq!(idx.find("pets/demo/pet.json").unwrap().crc32, 0x297c_0ff4);
        // `read` 内部逐条验 CRC ⇒ 这三行不报错本身就是跨实现 CRC 一致的证据
        let pet = idx.read("pets/demo/pet.json").unwrap();
        let text = String::from_utf8(pet).expect("夹具是 UTF-8");
        assert!(text.contains("\"demo\""), "{text}");
        let acc = String::from_utf8(idx.read("accessories/hat/accessory.json").unwrap()).unwrap();
        assert!(acc.contains("红帽"), "非 ASCII 内容要原样读回来");
        assert!(idx.find("accessories/").is_none(), "目录条目不算可得文件");
    }

    /// 改一个字节 ⇒ CRC 必须报。这条是"读侧不会把坏字节当资产返回"的唯一证据。
    #[test]
    fn a_single_flipped_byte_is_caught_by_crc() {
        let dir = scratch("crc_tamper");
        let mut bytes = one_entry_zip();
        // 数据段起点 = 中央目录里那条 data_offset；偏移算错的话这条测试会假绿，
        // 所以先断言"改的确实是数据"：改完读回来必须和原文不同。
        let off = {
            let f = put(&dir, "before.zip", &bytes);
            ZipIndex::open(&f).unwrap().entries()[0].data_offset as usize
        };
        bytes[off] ^= 0xFF;
        let f = put(&dir, "after.zip", &bytes);
        let e = ZipIndex::open(&f).expect("索引本身没坏，坏的是数据");
        let err = e
            .read("data/pwn.txt")
            .expect_err("改过一个字节必须被 CRC 拦住");
        let msg = err.message();
        assert!(msg.contains("CRC"), "{msg}");
        assert!(msg.contains("data/pwn.txt"), "要点名是哪个条目: {msg}");
    }

    /// 三种越界名字（穿越 / 绝对 / 空段）各测一次：**整个包出局**，不是跳过那一条。
    /// "跳过坏条目"产出的是一种比失败更糟的状态 —— 装了一个我们不知道长什么样的包。
    #[test]
    fn unsafe_names_reject_the_whole_pack() {
        let dir = scratch("unsafe_names");
        for (i, bad) in ["../x/pwn.txt", "/tmp/pwn.txt", "a//b/pwn.txt"]
            .iter()
            .enumerate()
        {
            let mut bytes = one_entry_zip();
            sub_all(&mut bytes, b"data/pwn.txt", bad.as_bytes());
            let f = put(&dir, &format!("bad{i}.zip"), &bytes);
            let err = ZipIndex::open(&f).expect_err("越界名字必须让整包失败");
            let ZipError::UnsafeName { ref name, .. } = err else {
                panic!("越界名字要报 UnsafeName，实际 {err:?}");
            };
            assert_eq!(name, *bad, "错误要点名是哪一条");
            let msg = err.message();
            assert!(msg.contains("整个包拒绝"), "要说清后果: {msg}");
        }
    }

    #[test]
    fn a_symlink_entry_is_rejected() {
        let dir = scratch("symlink");
        let mut bytes = one_entry_zip();
        let r = first_record(&bytes);
        // 外部属性高 16 位 = Unix mode；0o120777 = S_IFLNK + 0777
        bytes[r + 38..r + 42].copy_from_slice(&(0xA1ED_0000u32).to_le_bytes());
        let f = put(&dir, "link.zip", &bytes);
        let err = ZipIndex::open(&f).expect_err("符号链接能指向包外任何位置，必须拒绝");
        assert!(matches!(err, ZipError::Symlink { .. }), "{err:?}");
        assert!(err.message().contains("符号链接"), "{}", err.message());
    }

    /// method≠0 要**报错并给出可照办的命令**，不是悄悄解压一半、也不是当没看见。
    #[test]
    fn a_deflated_entry_is_rejected_with_an_actionable_message() {
        let dir = scratch("deflate");
        let mut bytes = one_entry_zip();
        let r = first_record(&bytes);
        bytes[r + 10..r + 12].copy_from_slice(&8u16.to_le_bytes()); // deflate
        let f = put(&dir, "deflated.zip", &bytes);
        let err = ZipIndex::open(&f).expect_err("store-only 加载器不许解压");
        assert!(matches!(err, ZipError::Deflate { .. }), "{err:?}");
        let msg = err.message();
        assert!(msg.contains("zip -0"), "要给能照着敲的命令: {msg}");
    }

    /// 写侧同样拒绝越界名字：写器不是"把目录里有什么就写什么"的中转站。
    #[test]
    fn the_writer_refuses_to_produce_an_unsafe_name() {
        for bad in ["../x", "/abs/x", "a//b", "", "C:/x"] {
            let err = write_store_zip(&[(bad, b"x")]).expect_err("越界名字 {bad} 不该写得出来");
            assert!(
                matches!(err, ZipError::UnsafeName { .. }),
                "{bad} ⇒ {err:?}"
            );
        }
    }

    /// 条目按名字升序，而不是按打包者写进中央目录的顺序 ⇒ 列表是确定性的。
    /// 这条判据的形状与 `discover_accessories` 里那次排序一模一样，理由也一模一样。
    #[test]
    fn entries_are_sorted_by_name_not_by_pack_order() {
        let dir = scratch("sorted");
        let bytes = write_store_zip(&[
            ("z.txt", b"3"),
            ("a.txt", b"1"),
            ("m.txt", b"2"),
            ("a.txt", b"dup?"), // 同名两条：写侧不去重（那是装包门禁的活），读侧要稳定排序
        ])
        .unwrap();
        let f = put(&dir, "s.zip", &bytes);
        let names = || -> Vec<String> {
            ZipIndex::open(&f)
                .unwrap()
                .entries()
                .iter()
                .map(|e| e.name.clone())
                .collect()
        };
        let once = names();
        assert_eq!(once, ["a.txt", "a.txt", "m.txt", "z.txt"]);
        assert_eq!(once, names(), "两次打开顺序必须一样");
        let again = ZipIndex::open(&f).unwrap();
        assert_eq!(
            ZipIndex::open(&f).unwrap().entries(),
            again.entries(),
            "整条索引都要可重放"
        );
    }

    /// 空文件要能过（`polygon.json` 之类将来可能有 0 字节的占位，但更重要的是
    /// `len == 0` 是 `read_entry` 里唯一一条会跳过 seek/read 的分支 —— 没测过就等于没有）。
    #[test]
    fn a_zero_length_entry_round_trips() {
        let dir = scratch("zero_len");
        let bytes = write_store_zip(&[("empty.txt", b"")]).unwrap();
        let f = put(&dir, "z.zip", &bytes);
        let idx = ZipIndex::open(&f).unwrap();
        assert_eq!(idx.entries()[0].len, 0);
        assert_eq!(idx.entries()[0].crc32, 0);
        assert_eq!(idx.read("empty.txt").unwrap(), Vec::<u8>::new());
    }

    /// 目录条目：进索引（`list` 要数得清包里有什么），但不给数据、不可 `find`。
    #[test]
    fn directory_entries_are_indexed_but_not_readable() {
        let dir = scratch("dir_entry");
        let bytes = write_store_zip(&[("d/", b""), ("d/x.txt", b"hi")]).unwrap();
        let f = put(&dir, "d.zip", &bytes);
        let idx = ZipIndex::open(&f).unwrap();
        let d = idx.entries().iter().find(|e| e.name == "d/").unwrap();
        assert!(d.is_dir);
        assert_eq!(idx.with_prefix("d/").len(), 1, "前缀查询只给文件");
        assert!(idx.find("d/").is_none());
    }

    /// 不是 zip / 被截断 ⇒ 都要报错，且**不能返回一个"少了几条但看着正常"的索引**。
    #[test]
    fn garbage_and_truncation_are_errors_not_partial_indexes() {
        let dir = scratch("bad_input");
        let f = put(
            &dir,
            "not.zip",
            b"this is not a zip file at all, really not.",
        );
        let err = ZipIndex::open(&f).expect_err("伪 zip 必须报错");
        assert!(matches!(err, ZipError::NoEocd), "{err:?}");

        let full = one_entry_zip();
        let f = put(&dir, "tiny.zip", b"short");
        assert!(matches!(ZipIndex::open(&f).unwrap_err(), ZipError::NoEocd));

        for cut in [1, 40, full.len() - 6] {
            let f = put(&dir, &format!("cut{cut}.zip"), &full[..cut]);
            let err = ZipIndex::open(&f).expect_err("截断到 {cut} 字节必须报错");
            let msg = err.message();
            assert!(
                msg.contains("EOCD") || msg.contains("中央目录") || msg.contains("读取失败"),
                "截断要说清坏在哪: {msg}"
            );
        }

        let err = ZipIndex::open(&dir.join("absent.zip")).expect_err("文件不存在必须报错");
        assert!(matches!(err, ZipError::Io(..)), "{err:?}");
        assert!(
            err.message().starts_with("zip 读取失败"),
            "{}",
            err.message()
        );
    }

    /// 本地头的 `extra_len` **可以和中央目录不一样**（Info-ZIP 打时间戳 extra 时就是这么干的），
    /// 那就必须有一条夹具把"数据起点只能从本地头算"这条走一遍 —— 否则那个分支永远没被执行过
    /// （假开关同族）。构造方法 = 往本地头里插 4 字节 extra，同时把 EOCD 的中央目录偏移抬 4，
    /// 其余字段一个不动 ⇒ 这是一份**合法的**包，只是两侧声明的头部长度不同。
    #[test]
    fn a_local_extra_field_shifts_the_data_and_is_still_read() {
        let dir = scratch("local_extra");
        let mut bytes = one_entry_zip();
        let local = central_off(&bytes) - (LOCAL_FIXED + 12 + 8);
        assert_eq!(
            u32::from_le_bytes([
                bytes[local],
                bytes[local + 1],
                bytes[local + 2],
                bytes[local + 3]
            ]),
            SIG_LOCAL
        );
        assert_eq!(
            u16::from_le_bytes([bytes[local + 28], bytes[local + 29]]),
            0
        );
        bytes.splice(
            local + LOCAL_FIXED + 12..local + LOCAL_FIXED + 12,
            [b'U', b'T', 4, 0],
        );
        bytes[local + 28..local + 30].copy_from_slice(&4u16.to_le_bytes());
        let e = bytes.len() - 22;
        let shifted = central_off(&bytes) as u32 + 4;
        bytes[e + 16..e + 20].copy_from_slice(&shifted.to_le_bytes());

        let f = put(&dir, "lendiff.zip", &bytes);
        let idx = ZipIndex::open(&f).unwrap();
        // 数据仍应是 "payload!"：按中央目录的 extra_len=0 去算会少读 4 字节，
        // 拿到的首字节是 'U' ⇒ CRC 立刻把它拦下来（这条断言因此不是自洽，是真的在选分支）
        assert_eq!(idx.read("data/pwn.txt").unwrap(), b"payload!");
    }
}
