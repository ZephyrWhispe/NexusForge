import { beforeAll, describe, expect, it } from "vitest";

/**
 * R-I4 按需装配注册面门禁（真机冒烟抓出的真缺陷门禁化）：
 * monaco 0.52 语言服务贡献（ts/css/html）只挂 onLanguage 钩子，语言 ID 与
 * monarch 着色器一律由 basic-languages 注册——按需装配漏登任一门，该语言
 * 静默无着色无服务，且体积/扫词/mock 夹具三类静态判据全部测不到。
 * 本测试不 mock setup，直载真 monaco：以 languageForPath 源码映射表的值域
 * 为唯一事实源，断言每一枚 ID 都在 monaco 注册表里真实存在。
 *
 * 形制说明：仓内其余测试按约定 mock monaco/setup（夹具零感知纪律），本文件
 * 是全仓唯一直载点，刻意用运行时动态 import 把 monaco 模块图隔离在 beforeAll
 * 内——jsdom 缺 document.queryCommandSupported（editor.all 的 clipboard 贡献
 * 顶层即调），先补这枚真浏览器恒有的 API 再载，与 xterm/monaco stub 同谱的
 * 环境垫片，不改被测面。
 */

/** 从 setup.ts 源码提取 languageForPath 映射表的全部语言 ID（值域）；
 *  扫描面用 import.meta.glob ?raw（与 remotePermBridge/deferred 同谱，零 node:fs 依赖） */
const setupSources = Object.entries(
  import.meta.glob("../setup.ts", { query: "?raw", import: "default", eager: true }) as Record<
    string,
    string
  >,
);
function servedLanguageIds(): string[] {
  const src = setupSources.find(([path]) => path.endsWith("setup.ts"))?.[1];
  if (!src) throw new Error("setup.ts 源码未命中 glob");
  const mapBlock = /const map: Record<string, string> = \{([\s\S]*?)\n\s*\};/.exec(src);
  if (!mapBlock) throw new Error("languageForPath 映射表形状漂移（正则失配）");
  const ids = [...mapBlock[1].matchAll(/:\s*"([a-z]+)"/g)].map((m) => m[1]);
  return [...new Set(ids)];
}

type MonacoSetup = typeof import("../setup");

describe("monaco 按需装配注册面（R-I4 冒烟门禁化）", () => {
  let setup: MonacoSetup;

  // 60s＝实测（11s）的 5 倍余量：monaco 模块图冷载在共享 worker 池下可被拖长，
  // 阈值只防"真挂了"不防慢。
  beforeAll(async () => {
    // jsdom 未实现的剪贴板探测 API（Chrome/Edge/WebView2 恒有，真机冒烟已证）
    (document as unknown as { queryCommandSupported: (c: string) => boolean }).queryCommandSupported =
      () => true;
    setup = await import("../setup");
  }, 60_000);

  it("languageForPath 值域每一枚语言 ID 均真实注册（缺门＝静默无着色）", () => {
    const registered = new Set(setup.monaco.languages.getLanguages().map((l) => l.id));
    const missing = servedLanguageIds().filter((id) => !registered.has(id));
    expect(missing).toEqual([]);
  });

  it("plaintext 兜底恒注册（editor.api 内建，装配改动不得破坏）", () => {
    const registered = new Set(setup.monaco.languages.getLanguages().map((l) => l.id));
    expect(registered.has("plaintext")).toBe(true);
  });

  it("worker 路由 label 全部有真实注册（MonacoEnvironment.getWorker 分派不悬空）", () => {
    const registered = new Set(setup.monaco.languages.getLanguages().map((l) => l.id));
    for (const id of ["json", "css", "scss", "less", "html", "typescript", "javascript"]) {
      expect(registered.has(id), id).toBe(true);
    }
  });
});
