import { describe, expect, it } from "vitest";
// vite `?raw`：断言对象是构建器眼见的源文件本身（同上谱的 remotePermBridge / deferred 形制）。
import appSrc from "../App.tsx?raw";

// D-43 C4b：每个窗口只渲一枚角色，但静态导入会把七枚页面一起编进主窗首屏的 index chunk。
// 实测拆出后 index 由 145.98 kB 降到 97.26 kB（tools/check-bundle-size.mjs 预算 146 kB）。
// 体积门只在上限时报警，而当前余量 48 kB 足以悄悄吞回四枚 aux 页面——故按源码形态钉死：
// 六枚非 main 窗口必须经 lazy() 动态导入，MainWorkbench 必须保持静态（首屏不该多等一次请求）。

const AUX_ROLES = [
  "./windows/QuickPanel",
  "./theme/ThemePreview",
  "./windows/OverlayShot",
  "./windows/PinWindow",
  "./windows/LauncherWindow",
  "./windows/NoteBarWindow",
] as const;

/** 该模块在 App.tsx 里是否只经 `lazy(() => import("…"))` 装载（返回 true 才算拆出去了）。 */
function isLazyOnly(src: string, path: string): boolean {
  const dynamic = new RegExp(`lazy\\(\\s*\\(\\)\\s*=>\\s*import\\(\\s*"${path}"\\s*\\)\\s*\\)`);
  const staticImport = new RegExp(`^import\\s+[^;]*from\\s+"${path}"`, "m");
  return dynamic.test(src) && !staticImport.test(src);
}

describe("App.tsx 窗口角色分包（D-43 C4b index 体积）", () => {
  it("判定函数本身能分辨静态与动态导入（证明断言非恒真）", () => {
    expect(isLazyOnly('import A from "./windows/OverlayShot";', "./windows/OverlayShot")).toBe(false);
    expect(isLazyOnly('const A = lazy(() => import("./windows/OverlayShot"));', "./windows/OverlayShot")).toBe(true);
    expect(isLazyOnly('const A = lazy(() => import("./windows/PinWindow"));', "./windows/OverlayShot")).toBe(false);
  });

  it("六枚非 main 窗口全部走 lazy 动态导入", () => {
    for (const path of AUX_ROLES) expect(isLazyOnly(appSrc, path)).toBe(true);
  });

  it("主工作台保持静态导入，且所有角色共用一枚 Suspense", () => {
    expect(/^import MainWorkbench from "\.\/windows\/MainWorkbench";$/m.test(appSrc)).toBe(true);
    expect((appSrc.match(/<Suspense/g) ?? []).length).toBe(1);
    // fallback 必须为 null：覆盖层在内容就绪前本就不可见，spinner 会在其中闪一下。
    expect(appSrc).toContain("<Suspense fallback={null}>");
  });
});
