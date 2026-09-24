import { describe, expect, it } from "vitest";

/**
 * T-B7-25 宿主桥机检（09 §6.2 字面测名）：term 的 SFTP 属性弹窗写回权限位
 * 走 file 域唯一命令 fileRemoteChmod——term-core 与 commands/term.rs 零第二份
 * chmod 实现。与 term-core ssh.rs 里 T-B7-6 立、T-B7-25 翻正的 Rust 行范围钉
 * 同谱双保险：Rust 钉管函数形状对照，这里整 crate 扫实现形状词。
 * 扫描面用 import.meta.glob ?raw（与 deferred.test.tsx 同谱，零 node:fs 依赖）。
 */

const rustSources = Object.entries(
  import.meta.glob("../../../../crates/term-core/src/**/*.rs", {
    query: "?raw",
    import: "default",
    eager: true,
  }) as Record<string, string>,
);
const termCmd = Object.entries(
  import.meta.glob("../../../../src-tauri/src/commands/term.rs", {
    query: "?raw",
    import: "default",
    eager: true,
  }) as Record<string, string>,
);
const panel = Object.entries(
  import.meta.glob("../TerminalPanel.tsx", {
    query: "?raw",
    import: "default",
    eager: true,
  }) as Record<string, string>,
);

describe("term 权限位宿主桥（T-B7-25）", () => {
  it("termSftp_chmodGoesThroughHostBridge_notSecondImpl", () => {
    // 正对照：三面扫描本体都在场且真覆盖 SFTP 面（零命中≠啥也没扫）
    expect(rustSources.length, "term-core 源码扫面不得为空").toBeGreaterThanOrEqual(8);
    expect(termCmd.length, "term 命令面扫面落空").toBe(1);
    expect(panel.length, "term 面板扫面落空").toBe(1);
    const all = rustSources.map(([, src]) => src).join("\n");
    expect(all, "扫面须含真 SFTP 动词本体").toContain("fn sftp_rename");
    expect(panel[0][1]).toContain("termSftpList");

    for (const [f, src] of rustSources) {
      expect(src, `${f} 长出 setperm 形制`).not.toMatch(/setperm/i);
      // 只禁实现形状（fn 名 / RPC 动词臂），注释里讲历史的"chmod"字样不误伤
      expect(src, `${f} 长出第二份 chmod 实现`).not.toMatch(/fn\s+\w*chmod|SshRpc::[A-Za-z]*[Cc]hmod/);
    }
    expect(termCmd[0][1], "term 命令面不得长出 chmod 命令").not.toMatch(
      /fn term_\w*chmod|setperm/i,
    );

    // 宿主桥落点：term 面板直调 file 域唯一命令（唯一 chmod 口的消费证据）
    expect(panel[0][1], "属性写回必须经 fileRemoteChmod 唯一口").toContain("fileRemoteChmod");
    expect(panel[0][1]).toContain("fileRemoteDrivers");
  });
});
