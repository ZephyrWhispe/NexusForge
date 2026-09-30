import { beforeEach, describe, expect, it, vi } from "vitest";

import { isNotesTab, isSysTab, isTermTab, useSession } from "../session";
import { SUBNAV } from "../../layout/modules";

// D-43 C6 补正的一枚诚实账：C5 的提交信息写着"termTab 入 partialize"，而文件里那一键从未落——
// 实跑快照证明当时它确实不在。本档把三枚视图键（termTab/sysTab/notesTab）的持久化往返钉成判据，
// 让"切走再回来不丢"这句承诺只对真在快照里的键成立（口径同 sessionClipView.test.ts）。

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
  useSession.setState({
    termTab: "sessions",
    sysTab: "monitor",
    notesTab: "notes",
    themeMode: "auto",
  });
});

describe("视图键持久化往返（D-43 C5/C6/C7）", () => {
  it("viewKeys_selectionsLandInTheSnapshot", () => {
    useSession.getState().setTermTab("docker");
    useSession.getState().setSysTab("pkg");
    useSession.getState().setNotesTab("canvas");
    expect(snapshot().termTab, "C5 声称入 partialize 却从未落——本键缺席即红").toBe("docker");
    expect(snapshot().sysTab).toBe("pkg");
    expect(snapshot().notesTab, "C7 的第三枚视图键：缺席即红").toBe("canvas");
    // 正对照防空洞：同一快照里的既有键仍在（partialize 没被改坏）
    expect(snapshot().themeMode).toBe("auto");

    // 野值既不落 store 也不改快照
    useSession.getState().setSysTab("nope");
    useSession.getState().setTermTab("nope");
    useSession.getState().setNotesTab("nope");
    expect([
      useSession.getState().termTab,
      useSession.getState().sysTab,
      useSession.getState().notesTab,
    ]).toEqual(["docker", "pkg", "canvas"]);
    expect(snapshot().termTab).toBe("docker");
    expect(snapshot().sysTab).toBe("pkg");
    expect(snapshot().notesTab).toBe("canvas");
  });

  it("viewKeys_legacySnapshot_missingBothKeys_zeroMigration", async () => {
    // C5/C6/C7 之前的快照没有这几枚键：重开模块（=应用重启）取初始值，其余旧键照常生效
    localStorage.setItem(
      KEY,
      JSON.stringify({ state: { themeMode: "dark", activeModule: "sys", clipGroup: "code" } }),
    );
    vi.resetModules();
    const fresh = (await import("../session")).useSession;
    await settled(fresh);
    const s = fresh.getState();
    expect(s.termTab).toBe("sessions");
    expect(s.sysTab).toBe("monitor");
    expect(s.notesTab).toBe("notes");
    expect(s.clipGroup).toBe("code");
    expect(s.themeMode).toBe("dark");
  });

  it("viewKeys_matchSubnavRegistry", () => {
    // 左轨「视图」条目与 store 联合一一对应：注册表里出现撒谎 id 即红
    for (const [module, guard] of [
      ["term", isTermTab],
      ["sys", isSysTab],
      ["notes", isNotesTab],
    ] as const satisfies readonly (readonly [keyof typeof SUBNAV, (v: string) => boolean])[]) {
      const ids = SUBNAV[module]
        .flatMap((sec) => sec.items)
        .filter((i) => (i.scope ?? "filter") === "view")
        .map((i) => i.id);
      expect(ids.length, `${module} 的左轨视图条目为空`).toBeGreaterThan(0);
      for (const id of ids) expect(guard(id), `SUBNAV 视图项 ${id} 不在联合`).toBe(true);
    }
    // 负例：他模块的视图 id 不混进本联合（否则两模块的选择态重新互污）
    expect(isSysTab("sessions")).toBe(false);
    expect(isTermTab("monitor")).toBe(false);
    expect(isSysTab("overview")).toBe(false);
    expect(isNotesTab("monitor")).toBe(false);
    expect(isSysTab("canvas")).toBe(false);
  });
});
