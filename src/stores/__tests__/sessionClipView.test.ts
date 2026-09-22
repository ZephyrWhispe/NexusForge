import { beforeEach, describe, expect, it, vi } from "vitest";

import { isClipView, useSession } from "../session";
import { SUBNAV } from "../../layout/modules";

// D-29 B3/T-B3-1 回归（09 §8.2）：clipView 入 partialize 即跨重启存活，旧快照缺键零迁移
// 回落默认；写侧拒野生 id；「视图」与「筛选」两维度分键、互不覆写（session.ts 既有红线延续）。

const KEY = "nf-session";

function snapshot(): Record<string, unknown> {
  const raw = localStorage.getItem(KEY);
  return (JSON.parse(String(raw)) as { state: Record<string, unknown> }).state;
}

/** 等 persist 水合落定（localStorage 源是同步的，但 hydrate 走的是 Promise 链） */
async function settled(store: typeof useSession): Promise<void> {
  if (store.persist.hasHydrated()) return;
  await new Promise<void>((resolve) => {
    const off = store.persist.onFinishHydration(() => {
      off();
      resolve();
    });
  });
}

beforeEach(() => {
  localStorage.clear();
  useSession.setState({ clipView: "history", clipGroup: "all", themeMode: "auto" });
});

describe("clipView 持久化往返（T-B3-1）", () => {
  it("clipView_selectionPersistedRoundtrip：写→快照含键、野值不落", () => {
    // 真 store 路径写入 → 持久化快照真的带上这一键（不是靠面板侧兜底假装存活）
    useSession.getState().setClipView("secret");
    expect(useSession.getState().clipView).toBe("secret");
    expect(snapshot().clipView).toBe("secret");
    // 正对照防空洞：同一快照里的既有键仍在（partialize 没被改坏）
    expect(snapshot().themeMode).toBe("auto");

    // 野值（手改快照/未来重命名的残留）既不落 store 也不改快照
    useSession.getState().setClipView("nope");
    expect(useSession.getState().clipView).toBe("secret");
    expect(snapshot().clipView).toBe("secret");
  });

  it("clipView_legacySnapshot_missingKey_zeroMigration", async () => {
    // B3 之前的快照没有 clipView 键：重开模块（=应用重启）取初始值，其余旧键照常生效
    localStorage.setItem(
      KEY,
      JSON.stringify({ state: { themeMode: "dark", activeModule: "proxy", clipGroup: "code" } }),
    );
    vi.resetModules();
    const fresh = (await import("../session")).useSession;
    await settled(fresh);
    const s = fresh.getState();
    expect(s.clipView).toBe("history");
    expect(s.clipGroup).toBe("code");
    expect(s.themeMode).toBe("dark");
  });

  it("clipPanel_twoScopes_neverOverwriteEachOther：换视图不动筛选，换筛选不动视图", () => {
    useSession.getState().setClipView("stack");
    useSession.getState().setClipGroup("url");
    expect([useSession.getState().clipView, useSession.getState().clipGroup]).toEqual([
      "stack",
      "url",
    ]);
    // 代理第三键同理分道（T-B2-3 红线不因新维度回归）
    useSession.getState().setProxySubPanel("nodes");
    expect([useSession.getState().clipView, useSession.getState().clipGroup]).toEqual([
      "stack",
      "url",
    ]);
  });

  it("clipView_idsMatchSubnavRegistry：注册表 view 项与 store 联合一一对应", () => {
    const ids = SUBNAV.clipboard
      .flatMap((sec) => sec.items)
      .filter((i) => (i.scope ?? "filter") === "view")
      .map((i) => i.id);
    expect(ids.length).toBeGreaterThan(0);
    for (const id of ids) expect(isClipView(id), `SUBNAV 视图项 ${id} 不在 ClipView 联合`).toBe(true);
    // 负例：筛选维度/代理子面板的 id 不混进视图联合（否则两维度语义重新互污）
    expect(isClipView("all")).toBe(false);
    expect(isClipView("overview")).toBe(false);
  });
});
