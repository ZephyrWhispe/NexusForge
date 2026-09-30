import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import TerminalPanel from "../TerminalPanel";
import { SUBNAV } from "../../../layout/modules";
import { isTermTab, useSession } from "../../../stores/session";
import {
  termDockerContainers,
  termSessions,
  termSftpList,
  termWslList,
  type DockerContainerDto,
  type SftpEntryDto,
  type TermSessionDto,
} from "../../../ipc/client";

// D-43 C5：终端面板本体重排的判据面。四件事各钉一枚——
// ① 左轨注册表与面板视图门控对合（SUBNAV.term 的 id 必须全是 TermTab，否则左轨出现
//    点了没反应的撒谎按钮；互斥视图不在 DOM，故 term 用 view 形制而非 anchor）；
// ② 工具条分区次序即规范 5 节判据：主按钮「新建本地终端」恒末位；
// ③ 内层 maxHeight 视口撤除后长清单靠分页显影，页脚报"共 n · 在场 k"而非静默截断；
// ④ 空态诚实：Docker 未拉取 ≠ 无容器，两句话分别是两句事实。
// 像素/滚动一律不在此证明（jsdom 无布局引擎），归 C9 真机 CDP 走查。

vi.mock("@xterm/xterm", () => {
  class Terminal {
    element?: HTMLElement;
    loadAddon() {}
    open(el: HTMLElement) {
      this.element = el;
    }
    write() {}
    dispose() {}
    onData() {
      return { dispose() {} };
    }
    onResize() {
      return { dispose() {} };
    }
  }
  return { Terminal };
});
vi.mock("@xterm/addon-fit", () => {
  class FitAddon {
    fit() {}
    dispose() {}
  }
  return { FitAddon };
});

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    termSessions: vi.fn(),
    termWslList: vi.fn(),
    termDockerContainers: vi.fn(),
    termSftpList: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

const sshSession: TermSessionDto = {
  id: "s1",
  kind: { kind: "ssh", host: "h.example", port: 22, user: "root" },
  title: "root@h.example",
  alive: true,
  cols: 80,
  rows: 24,
};

function container_(id: string, state: string): DockerContainerDto {
  return { id, name: id, image: "img", status: "up", state };
}

let container: HTMLDivElement;
let root: Root;

function buttonByText(text: string, scope: ParentNode = document): HTMLButtonElement | undefined {
  return [...scope.querySelectorAll("button")].find((b) => b.textContent?.trim() === text);
}

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<TerminalPanel />);
  });
  await act(async () => {});
}

/** 区块根（Section 自带 data-nf="sec"），标题＝head 行首个 Text（span），不含操作区文案 */
function sectionTitles(): string[] {
  return [...container.querySelectorAll('[data-nf="sec"]')].map(
    (el) => el.querySelector("span")?.textContent?.trim() ?? "",
  );
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(termSessions).mockResolvedValue([]);
  vi.mocked(termWslList).mockResolvedValue([]);
  vi.mocked(termDockerContainers).mockResolvedValue([]);
  vi.mocked(termSftpList).mockResolvedValue([]);
  act(() => useSession.getState().setTermTab("sessions"));
});

afterEach(() => {
  act(() => {
    try {
      root?.unmount();
    } catch {
      /* 用例内已卸载 */
    }
  });
  container.remove();
  vi.clearAllMocks();
});

describe("TerminalPanel 重排（D-43 C5）", () => {
  it("termLayout_registryCoversGatedViews_andMutualExclusionHolds", async () => {
    const items = SUBNAV.term.flatMap((s) => s.items);
    expect(items.map((i) => i.scope)).toEqual(["view", "view"]);
    for (const i of items) expect(isTermTab(i.id), `${i.id} 不是 TermTab`).toBe(true);

    await mount();
    // 缺省档 sessions：三区块各有标题（首层无题＝"直接放在页面上"的形态学根因）
    expect(sectionTitles()).toEqual(["SSH 连接", "终端", "远端文件（SFTP）"]);
    expect(document.body.textContent).not.toContain("容器（Docker Desktop 需运行中）");

    // 切档走 session 分键（左轨派发 setTermTab，面板读同一键）
    act(() => useSession.getState().setTermTab("docker"));
    await act(async () => {});
    expect(useSession.getState().termTab).toBe("docker");
    expect(sectionTitles()).toEqual(["容器（Docker Desktop 需运行中）"]);
    expect(document.querySelector('[placeholder="SSH 主机"]'), "互斥视图不得留在 DOM").toBeNull();

    // 野值不落（持久化快照被改坏时确定性回落，不白屏）
    act(() => useSession.getState().setTermTab("nope"));
    expect(useSession.getState().termTab).toBe("docker");
  });

  it("termLayout_toolbarPrimaryIsLastAndSshEntriesScopedToSection", async () => {
    vi.mocked(termSessions).mockResolvedValue([sshSession]);
    await mount();
    const toolbars = container.querySelectorAll('[role="toolbar"]');
    expect(toolbars, "工具条唯一（会话切换与启动入口都在其中）").toHaveLength(1);
    const bar = toolbars[0];
    const buttons = [...bar.querySelectorAll("button")];
    expect(buttons[buttons.length - 1]?.textContent?.trim(), "主按钮恒末位（规范 5 节）").toBe(
      "新建本地终端",
    );
    // 在场会话组进工具条左区：切哪条会话即看哪块终端
    expect(bar.querySelector('[aria-label="在场会话"]'), "会话组应在工具条内").not.toBeNull();
    // 三枚 SSH 对话框入口随「SSH 连接」区块头出场（动作对象是 SSH 会话，Docker 档里无处可用；
    // 且工具条挤不下时按钮折两行＝破 §10-15，真窗实测 55.6 高）
    const sshSection = [...container.querySelectorAll('[data-nf="sec"]')].find(
      (el) => el.querySelector("span")?.textContent?.trim() === "SSH 连接",
    )!;
    for (const label of ["已知主机", "一次性远端命令", "端口转发"]) {
      const btn = buttonByText(label, sshSection);
      expect(btn, `${label} 应在 SSH 区块头`).toBeDefined();
      expect(bar.contains(btn!), `${label} 不该挤进工具条（折行破 §10-15）`).toBe(false);
    }
    // 工具条是视图无关的（切到 Docker 仍能关会话/起新终端），区块头入口则随 SSH 档一起退场
    act(() => useSession.getState().setTermTab("docker"));
    await act(async () => {});
    expect(container.querySelectorAll('[role="toolbar"]'), "工具条不随视图消失").toHaveLength(1);
    expect(buttonByText("已知主机"), "区块头入口不得留在 Docker 档（那里无处可用）").toBeUndefined();
  });

  it("termLayout_sftpPagination_footerCountsAndNoSilentTruncation", async () => {
    vi.mocked(termSessions).mockResolvedValue([sshSession]);
    const entries: SftpEntryDto[] = Array.from({ length: 41 }, (_, i) => ({
      name: `f${i}`,
      is_dir: false,
      size: 1024,
    }));
    vi.mocked(termSftpList).mockResolvedValue(entries);
    await mount();

    await click(container.querySelector('[aria-label="在场会话"] [role="button"]')!);
    await click(buttonByText("SFTP 浏览")!);
    expect(termSftpList).toHaveBeenCalledTimes(1);

    // 首屏档＝40 行（内层 320px 视口撤除后的替代形制）
    expect(container.querySelectorAll('[data-sftp-entry]'), "首屏应落 40 行").toHaveLength(40);
    expect(document.body.textContent).toContain("共 41 项 · 在场 40 项");
    const more = buttonByText("显示更多");
    expect(more, "超出首屏必须给出口，不得静默截断").toBeDefined();
    await click(more!);
    expect(container.querySelectorAll('[data-sftp-entry]')).toHaveLength(41);
    expect(buttonByText("显示更多")).toBeUndefined();
    expect(document.body.textContent).toContain("共 41 项 · 在场 41 项");
  });

  it("termLayout_dockerFooter_notPulledDiffersFromEmpty", async () => {
    act(() => useSession.getState().setTermTab("docker"));
    await mount();
    // 未拉取：说"尚未拉取"，不说"无容器"（后者是 Engine 不可达的结论，混淆即撒谎）
    expect(termDockerContainers).not.toHaveBeenCalled();
    expect(document.body.textContent).toContain("尚未拉取容器列表");
    expect(document.body.textContent).not.toContain("无容器（或 Docker Engine 不可达）");

    await click(buttonByText("刷新")!);
    vi.mocked(termDockerContainers).mockResolvedValue([
      container_("web", "running"),
      container_("db", "exited"),
    ]);
    await click(buttonByText("刷新")!);
    expect(document.body.textContent).toContain("共 2 个容器");
    // 静默截断显影：tail 行数口径写进页脚而非只存在于后端
    expect(document.body.textContent).toContain("日志取 tail 末 200 行");
  });
});
