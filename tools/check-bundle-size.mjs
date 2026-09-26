// PERF-03 产物体积预算（docs/review-2026-09-25/07 §3；D-38 R-J3）：
// 读 dist/assets/*.js，按 chunk 族核对上限。基线＝R-I4 monaco 按需装配收口实测
// （2026-09-26：monaco 3,423.44 kB / fluentui 642.57 / xterm 288.63 / vendor 138.37 /
// index 136.15 kB），上限＝基线 +~6% 防抖余量——体积回弹（如按需装配被回退、
// 依赖树膨胀）即红，把"monaco chunk 必须保持装配后量级"钉成常驻门禁。
// 新增 chunk 族不入预算（懒加载面板自然增长是预期），但改名/失踪必报。
import { readdir, stat } from 'node:fs/promises';
import { join } from 'node:path';

const BUDGETS_KB = {
  monaco: 3630,
  fluentui: 685,
  xterm: 308,
  vendor: 148,
  index: 146,
};

// 单位口径＝十进制 kB，与 vite 报告（以及上方基线数值）逐字可比
const KB = 1000;
const assets = 'dist/assets';
let fail = 0;
const seen = {};
for (const name of await readdir(assets)) {
  if (!name.endsWith('.js')) continue;
  const m = /^(.+)-[A-Za-z0-9_-]{6,}\.js$/.exec(name);
  if (!m) continue;
  const family = m[1];
  const { size } = await stat(join(assets, name));
  seen[family] = (seen[family] ?? 0) + size / KB;
}
for (const [family, budget] of Object.entries(BUDGETS_KB)) {
  const kb = seen[family];
  if (kb === undefined) {
    console.error(`FAIL 预算 chunk 族「${family}」在 dist 中失踪（改名/分包漂移，随源核账后更新本表）`);
    fail = 1;
    continue;
  }
  const line = `  ${family.padEnd(9)} ${kb.toFixed(2).padStart(9)} kB / 预算 ${budget} kB`;
  if (kb > budget) {
    console.error(`FAIL 体积超预算：${line}`);
    fail = 1;
  } else {
    console.log(`ok   ${line.trim()}`);
  }
}
const others = Object.entries(seen)
  .filter(([f]) => !(f in BUDGETS_KB))
  .sort((a, b) => b[1] - a[1])
  .slice(0, 3);
for (const [f, kb] of others) console.log(`  （非预算最大者：${f} ${kb.toFixed(2)} kB）`);
process.exit(fail);
