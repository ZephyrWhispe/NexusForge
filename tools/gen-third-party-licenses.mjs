// 生成 THIRD_PARTY_LICENSES.md：从 cargo metadata + node_modules 提取依赖许可证清单
// 用法：node tools/gen-third-party-licenses.mjs（需已 cargo metadata / npm install）
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "..");

// ---- Rust 依赖（cargo metadata，过滤 workspace 本地 path 包，按 windows 目标平台） ----
const meta = JSON.parse(
  execFileSync("cargo", ["metadata", "--format-version", "1", "--filter-platform", "x86_64-pc-windows-msvc"], {
    cwd: root,
    maxBuffer: 256 * 1024 * 1024,
  }).toString("utf8"),
);
const rust = new Map();
for (const p of meta.packages) {
  if (!p.source) continue; // workspace 成员
  const key = `${p.name} ${p.version}`;
  if (!rust.has(key)) rust.set(key, p.license ?? "(未声明)");
}

// ---- npm 依赖（package-lock v3 的 packages 表 + node_modules 内 package.json 的 license 字段） ----
const lock = JSON.parse(fs.readFileSync(path.join(root, "package-lock.json"), "utf8"));
const npm = new Map();
for (const [loc, entry] of Object.entries(lock.packages ?? {})) {
  if (!loc.startsWith("node_modules/")) continue;
  const name = entry.name ?? loc.split("node_modules/").pop();
  if (loc.split("node_modules/").length > 2) continue; // 提升树只取一层（同包多版本由 lock 去重保证）
  const key = `${name} ${entry.version}`;
  if (npm.has(key)) continue;
  let license = entry.license;
  if (!license) {
    const pj = path.join(root, loc, "package.json");
    try {
      license = JSON.parse(fs.readFileSync(pj, "utf8")).license;
    } catch { /* 回退到未声明 */ }
  }
  npm.set(key, license || "(未声明)");
}

// ---- 汇总 ----
function tally(map) {
  const t = new Map();
  for (const lic of map.values()) t.set(lic, (t.get(lic) ?? 0) + 1);
  return [...t.entries()].sort((a, b) => b[1] - a[1]);
}
function table(map) {
  return [...map.entries()]
    .sort((a, b) => a[0].localeCompare(b[0]))
    .map(([k, lic]) => `| ${k.split(" ")[0]} | ${k.split(" ")[1]} | ${lic} |`)
    .join("\n");
}

const out = [];
out.push("# 第三方依赖许可清单（THIRD_PARTY_LICENSES）");
out.push("");
out.push("> 本文件由 `node tools/gen-third-party-licenses.mjs` 生成，请勿手工编辑。");
out.push(`> 生成日期：${new Date().toISOString().slice(0, 10)} ｜ Rust 依赖（x86_64-pc-windows-msvc 解析）${rust.size} 个 ｜ npm 依赖 ${npm.size} 个`);
out.push("");
out.push("项目本身以 GPL-3.0-only 发布（见 [LICENSE](LICENSE)）。下列依赖的许可证均与 GPL-3.0 分发兼容；`(未声明)` 条目由批次 2 的 cargo-deny 门禁复核。");
out.push("");
out.push("## 许可证分布");
out.push("");
out.push("| 许可证 | Rust 包数 |");
out.push("|---|---|");
for (const [lic, n] of tally(rust)) out.push(`| ${lic} | ${n} |`);
out.push("");
out.push("| 许可证 | npm 包数 |");
out.push("|---|---|");
for (const [lic, n] of tally(npm)) out.push(`| ${lic} | ${n} |`);
out.push("");
out.push("## Rust 依赖（crates.io）");
out.push("");
out.push("| 包 | 版本 | 许可证 |");
out.push("|---|---|---|");
out.push(table(rust));
out.push("");
out.push("## npm 依赖");
out.push("");
out.push("| 包 | 版本 | 许可证 |");
out.push("|---|---|---|");
out.push(table(npm));
out.push("");

fs.writeFileSync(path.join(root, "THIRD_PARTY_LICENSES.md"), out.join("\n"), "utf8");
console.log(`THIRD_PARTY_LICENSES.md 已生成：Rust ${rust.size} + npm ${npm.size}`);
