//! 滚动截图的重叠检测与纵向拼接（D-29 B4 T-B4-5，纯函数零 IO）
//!
//! 形态：**手动步进**。用户自己滚一段、点一下"追加"，本模块把新帧与已拼好的带子对齐后
//! 接上去。全模块不产生任何输入注入（不 sendinput、不 wheel），因此"滚不动"这类
//! 目标应用行为永远不归我们背。
//!
//! 判错的代价决定了对齐的写法：拼错的产物是一张看起来完整的坏图（重叠区被当成新内容
//! 重复了一遍，或者中间被吃掉一截），比"拼不上"糟糕得多。所以候选偏移先用行签名快速筛，
//! 命中后再逐字节比对那一行的像素——64 位哈希的碰撞率不足以让一张图凭空对上去。

/// 一次对齐最多试探多少行。
///
/// 上限存在的理由不是省时间，而是**诚实**：画面里有一整片彼此全等的行（空白区、纯色背景，
/// 或者用户根本没滚）时，"最长连续相等"的字面解会把偏移一路推到整帧高——于是拼接器可以
/// 宣称"一次就拼完了"，下一帧一个像素都不追加，产物停在第一屏：用户看到的高度数字不动，
/// 手上没有任何线索说明为什么。取 `SCROLL_MAX_BAND` 为上界后，这类帧至多对齐这么多行，
/// 超出部分照直追加，产物会长，用户在步进条上看得见它长。
///
/// 与之相对，行与行各不相同的静止两帧在这条路上得到的是 `None`（对不上任何一段），
/// 由调用侧原样分段——那种帧没有任何全等行可推，硬给一个偏移就是猜。
pub const SCROLL_MAX_BAND: u32 = 240;

/// 一次会话最多追加几帧（含 `scroll_begin` 的首帧之后）。
///
/// 内存上界：一条带子的像素 = 宽 × 累计高 × 4，全屏宽 4K 下单帧就 ~33MB。
/// 段数不设限时用户可以一路点到浏览器把整条虚拟桌面的像素都留在内存里，
/// 而这个上限在覆盖层上只是一句"到此为止"的提示——不做有损丢弃（丢掉某帧来腾地方
/// 会让用户以为拼出来的长图是完整的）。
pub const SCROLL_MAX_STEPS: u32 = 20;

/// 一条纵向带子（拼接的中间产物，与 [`host_core::ports::Frame`] 的区别只有一个：
/// 这里已经是 RGBA、且坐标系原点在该带子自己）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrollBand {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl ScrollBand {
    pub fn empty(width: u32) -> Self {
        Self {
            width,
            height: 0,
            rgba: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.height == 0 || self.rgba.is_empty()
    }

    fn row_bytes(&self) -> usize {
        self.width as usize * 4
    }

    /// 第 `y` 行的字节（越界给空片：调用侧的行号全部来自 `0..height`，这里不重复检查）
    fn row(&self, y: u32) -> &[u8] {
        let rb = self.row_bytes();
        let start = y as usize * rb;
        &self.rgba[start..start + rb]
    }
}

/// 一行的 FNV-1a 64 位签名。跨帧可比：同宽同内容 → 同签名。
///
/// 只作预筛用（见模块头），最终判定一律回到 [`stitch_v`] 之前的逐字节比对。
pub fn row_signature(rgba: &[u8], w: u32, y: u32) -> u64 {
    let rb = w as usize * 4;
    let start = y as usize * rb;
    // 尾行被截断（调用方给了个不满的缓冲）时按实际字节数签，不越界：
    // 这条路径在拼接器里走不到，但它一旦被走到，panic 会把一次抓帧变成一整条命令的失败
    let end = ((start + rb).min(rgba.len())).max(start);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in &rgba[start..end] {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn row_sigs(band: &ScrollBand, rows: impl Iterator<Item = u32>) -> Vec<u64> {
    rows.map(|y| row_signature(&band.rgba, band.width, y))
        .collect()
}

/// 找出 `next` 顶部与 `prev` 底部的重叠行数（对齐偏移）。
///
/// 返回 `None` = **对不上**，调用侧按降级处理（原样追加为独立段），而不是猜一个偏移。
/// 三种对不上：宽度不同（同一矩形重取却宽度变了 = 目标被改窗口大小/DPI 跳变）、
/// 任一带子为空、`1..=max_band` 内没有任何连续相等偏移。
pub fn find_overlap(prev: &ScrollBand, next: &ScrollBand, max_band: u32) -> Option<usize> {
    if prev.width != next.width || prev.width == 0 {
        return None;
    }
    if prev.is_empty() || next.is_empty() {
        return None;
    }
    let cap = max_band.min(prev.height).min(next.height) as usize;
    if cap == 0 {
        return None;
    }
    // prev 末 cap 行 / next 首 cap 行的签名，各算一次（内层比较全部查表）
    let prev_base = prev.height as usize - cap;
    let prev_sigs = row_sigs(prev, prev_base as u32..prev.height);
    let next_sigs = row_sigs(next, 0..cap as u32);
    let mut best = None;
    for d in 1..=cap {
        // 候选偏移 d：prev 的末 d 行 == next 的首 d 行
        let off = cap - d;
        if prev_sigs[off..] != next_sigs[..d] {
            continue;
        }
        // 签名全等只是"值得再看一眼"，逐字节确认后才算数（防哈希碰撞拼出错图）
        let confirmed = (0..d as u32).all(|k| prev.row(prev.height - d as u32 + k) == next.row(k));
        if confirmed {
            best = Some(d);
        }
    }
    best
}

/// 纵向拼接：`prev` 全文 + `next` 去掉前 `overlap` 行的尾巴。
///
/// 结果高度恒等于 `prev.height + next.height - overlap`，因此调用侧可以用它做
/// "追加了多少行"的对账（`scroll_append` 的 `height` 字段就是这么来的）。
pub fn stitch_v(prev: &ScrollBand, next: &ScrollBand, overlap: usize) -> ScrollBand {
    if prev.width != next.width {
        // 宽度不同的两帧不该进到这里（`find_overlap` 已按宽度拦），真走到了也不 panic：
        // 覆盖层里一次 panic 会连整个会话的已采帧一起带走，而这里最坏的正当结果只是
        // "这一帧没加上"——高度不变，用户在步进条上看得见它没变。
        return prev.clone();
    }
    let overlap = overlap.min(prev.height as usize).min(next.height as usize);
    let rb = prev.row_bytes();
    let mut rgba = Vec::with_capacity(((prev.height + next.height) as usize - overlap) * rb);
    rgba.extend_from_slice(&prev.rgba);
    rgba.extend_from_slice(&next.rgba[overlap * rb..]);
    ScrollBand {
        width: prev.width,
        height: prev.height + next.height - overlap as u32,
        rgba,
    }
}

/// `next` 相对 `prev` 实际新增的那截（步进条预览：只编码刚加上来的部分）。
///
/// 与 [`stitch_v`] 共用同一个 `overlap`，所以预览里看见的行数与产物里多出来的行数
/// 必然一致——两处各算一次重叠，就是"预览对了、保存错了"的成因。
pub fn tail_of(next: &ScrollBand, overlap: usize) -> ScrollBand {
    let overlap = overlap.min(next.height as usize);
    let rb = next.row_bytes();
    ScrollBand {
        width: next.width,
        height: next.height - overlap as u32,
        rgba: next.rgba[overlap * rb..].to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::{find_overlap, row_signature, stitch_v, tail_of, ScrollBand, SCROLL_MAX_BAND};

    /// 每行填成"行号自己的样子"：内容只由**全局行号**决定，于是"下移 n 行"就是
    /// 行号整体加 n，重叠对不对一眼可判，也不会有两行碰巧长一样。
    fn band(width: u32, first_row: u32, rows: u32) -> ScrollBand {
        let mut rgba = Vec::with_capacity((width * rows * 4) as usize);
        for r in 0..rows {
            let g = first_row + r;
            for _ in 0..width {
                rgba.extend_from_slice(&[g as u8, (g >> 8) as u8, 7, 255]);
            }
        }
        ScrollBand {
            width,
            height: rows,
            rgba,
        }
    }

    /// 每行都长一样的带子（空白区/纯色背景/整屏没动）：这种帧里"最长连续相等"没有
    /// 自然终点，正是 `SCROLL_MAX_BAND` 要拦住的那一类
    fn flat_band(width: u32, rows: u32) -> ScrollBand {
        let rgba = vec![9u8; (width * rows * 4) as usize];
        ScrollBand {
            width,
            height: rows,
            rgba,
        }
    }

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-5）字面测试名优先于 rustc 命名惯例
    fn scrollOverlap_findsShiftedByRowsBand() {
        // prev = 行 0..80；next = 行 40..140（下移 40 行 + 新 100 行）→ 重叠恰 40
        let prev = band(3, 0, 80);
        let next = band(3, 40, 100);
        assert_eq!(
            find_overlap(&prev, &next, SCROLL_MAX_BAND),
            Some(40),
            "偏移必须精确等于真实滚动行数：多算吃掉内容，少算重复内容"
        );

        // 负对照（同一套代码，只把滚动量改 1 行）：判据不是"返回了个四十几"
        assert_eq!(
            find_overlap(&prev, &band(3, 41, 100), SCROLL_MAX_BAND),
            Some(39)
        );
        // 正对照（重叠恰为 1 行）：下界也查得出来，不是只在大偏移上碰巧对
        assert_eq!(
            find_overlap(&prev, &band(3, 79, 20), SCROLL_MAX_BAND),
            Some(1)
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn scrollOverlap_differentWidth_returnsNoneNotPanic() {
        // 红线：宽度突变（中途改窗口大小 / DPI 跳变）如实降级，不猜对齐
        let prev = band(4, 0, 50);
        let next = band(5, 0, 50);
        assert_eq!(find_overlap(&prev, &next, SCROLL_MAX_BAND), None);
        // 空带子同臂：0 高帧既没内容可比，也不该被当成"重叠 0 行"接上去
        assert_eq!(
            find_overlap(&prev, &ScrollBand::empty(4), SCROLL_MAX_BAND),
            None
        );
        assert_eq!(
            find_overlap(&ScrollBand::empty(4), &next, SCROLL_MAX_BAND),
            None
        );

        // 正对照：同宽同内容时必须给得出重叠，否则上面三条可以是"永远返回 None"
        assert_eq!(
            find_overlap(&band(4, 0, 50), &band(4, 20, 50), SCROLL_MAX_BAND),
            Some(30)
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn scrollOverlap_identicalRows_isCappedNotClaimingFullMatch() {
        // 诚实边界：整帧彼此全等的行（用户根本没滚）→ 至多 SCROLL_MAX_BAND，绝不"整段重合"
        let still = flat_band(2, 600);
        assert_eq!(
            find_overlap(&still, &still, SCROLL_MAX_BAND),
            Some(SCROLL_MAX_BAND as usize),
            "全等行的重叠必须被上限拦住：宣称整帧重合会让下一帧一个像素都不追加"
        );
        // 上限拦的是"推到底的那个解"，不是拦掉追加本身：产物照直变长，用户在步进条上看得见
        assert_eq!(
            stitch_v(
                &still,
                &still,
                find_overlap(&still, &still, SCROLL_MAX_BAND).unwrap()
            )
            .height,
            960,
            "600 + 600 − 240：长出来的 360 行是看得见的进展，不是停在第一屏"
        );
        // 帧本身比上限还矮时，全等就是全等（上限不该反过来把真重叠切小）
        let short = flat_band(2, 10);
        assert_eq!(find_overlap(&short, &short, SCROLL_MAX_BAND), Some(10));
        // 正对照：把上限改小，同一对帧就按新上限走——判据在上限手里，不在数据里
        assert_eq!(find_overlap(&still, &still, 16), Some(16));

        // 另一头：行与行各不相同的静止两帧没有任何可推的重叠 → None（不猜偏移），
        // 由调用侧原样分段。上面那条 Some 证明这枚 None 不是"永远返回 None"的假绿。
        let distinct = band(2, 0, 600);
        assert_eq!(find_overlap(&distinct, &distinct, SCROLL_MAX_BAND), None);
    }

    #[test]
    #[allow(non_snake_case)]
    fn scrollStitch_appendsOnlyNewTailRows() {
        let prev = band(3, 0, 80);
        let next = band(3, 40, 100);
        let overlap = find_overlap(&prev, &next, SCROLL_MAX_BAND).unwrap();
        let out = stitch_v(&prev, &next, overlap);

        // 高度对账：h = h1 + h2 − overlap（= 全局行 0..140，不重不漏）
        assert_eq!(out.height, 80 + 100 - overlap as u32);
        assert_eq!(out.height, 140);
        assert_eq!(out.width, 3);
        assert_eq!(out.rgba.len(), 3 * 140 * 4);

        // 尾部像素逐字节等于 next 的非重叠区
        let tail = tail_of(&next, overlap);
        assert_eq!(tail.height, 100 - overlap as u32);
        assert_eq!(
            &out.rgba[(out.height as usize - tail.height as usize) * 12..],
            &tail.rgba[..]
        );
        // 前段原样保留（拼接不许回头改已入账的画面）
        assert_eq!(&out.rgba[..prev.rgba.len()], &prev.rgba[..]);
        // 行内容对账：第 100 行的签名 == 全局第 100 行（正对照，防"长度对但内容错"）
        assert_eq!(
            row_signature(&out.rgba, out.width, 100),
            row_signature(&band(3, 100, 1).rgba, 3, 0)
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn stitch_v_widthMismatch_keepsPrevUnchanged() {
        // 调用侧走不到这条（find_overlap 已按宽度拦），走到的话宁可"这一帧没加上"
        // 也不产出一张左右错位的图
        let prev = band(4, 0, 30);
        let out = stitch_v(&prev, &band(6, 0, 30), 0);
        assert_eq!(out, prev);
    }
}
