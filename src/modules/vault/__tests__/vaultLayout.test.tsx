import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import VaultPanel from "../VaultPanel";
import { SUBNAV } from "../../../layout/modules";
import {
  vaultEntries,
  vaultFolders,
  vaultStatus,
  type VaultEntryDto,
  type VaultFolderDto,
  type VaultStatusDto,
} from "../../../ipc/client";

// D-43 C4：保险库本体重排的判据面。
// 四件事各钉一枚——① 锚点式二级导航的注册表 id 必须与页面 [data-nf-sec] 一一对应（否则左轨
// 出现点了没反应的撒谎按钮）；② 三态同构骨架（未初始化/锁定/已解锁都渲染两枚区块），锚点
// 因此永不指向缺席的 DOM；③ 生成器长度由浏览器 number 微调钮换 Fluent SpinButton（原生控件
// 归零，layoutCompliance 台账同批摘行）；④ 后端已回传却从未上屏的 KDF 头部快照行内显影。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    vaultStatus: vi.fn(),
    vaultFolders: vi.fn(),
    vaultEntries: vi.fn(),
    vaultEntryAdd: vi.fn(),
    vaultEntryUpdate: vi.fn(),
    vaultGeneratePassword: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

const header = {
  version: 1,
  vault_id: "v1",
  kdf: { algo: "argon2id", m_cost_kib: 65536, t_cost: 3, p_cost: 1, salt_b64: "AA" },
  wrapped_dek: { nonce_b64: "AA", ct_b64: "AA" },
};

function statusOf(state: VaultStatusDto["state"]): VaultStatusDto {
  return {
    state,
    lockout_remaining_secs: 0,
    // 已解锁态才带头部快照（后端口径：未解锁不返回 KDF 参数）
    kdf: state === "unlocked" ? header : null,
    hello_enabled: false,
    hello_forced: false,
    hello_available: false,
  };
}

function folder(id: string, name: string): VaultFolderDto {
  return { id, name, created_at: 0 };
}

function entry(id: string, title: string): VaultEntryDto {
  return {
    id,
    folder_id: null,
    title,
    favorite: false,
    fields: [{ key: "password", kind: "password", value: "s3cret" }],
    totp_secret: null,
    created_at: 0,
    updated_at: 0,
  };
}

let container: HTMLDivElement;
let root: Root;

function buttonByText(text: string): HTMLButtonElement | undefined {
  return [...document.querySelectorAll("button")].find((b) => b.textContent?.trim() === text);
}

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
  await act(async () => {});
}

async function mount(status: VaultStatusDto) {
  vi.mocked(vaultStatus).mockResolvedValue(status);
  await act(async () => {
    root = createRoot(container);
    root.render(<VaultPanel search="" group="all" onCounts={() => {}} />);
  });
  await act(async () => {});
  await act(async () => {});
}

/** 页面里真实在场的锚点（与壳层 data-nf="work" 同族惯例，实窗 CDP 也按它枚举区块） */
function anchorIds(scope: ParentNode = container): string[] {
  return [...scope.querySelectorAll('[data-nf-sec]')].map((el) => el.getAttribute("data-nf-sec")!);
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(vaultFolders).mockResolvedValue([folder("f1", "工作")]);
  vi.mocked(vaultEntries).mockResolvedValue([entry("e1", "GitHub")]);
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

describe("VaultPanel 布局重排（D-43 C4）", () => {
  it("vault_registeredAnchorsEqualRenderedOnes：左轨每条锚点都有真落点，反向亦然", async () => {
    await mount(statusOf("unlocked"));
    const registered = SUBNAV.vault
      .flatMap((s) => s.items)
      .filter((i) => (i.scope ?? "filter") === "anchor")
      .map((i) => i.id)
      .sort();
    expect(anchorIds().sort()).toEqual(registered);
    // 两枚锚点都带可见标题（00 规范 1 节：首层区块不得无题）
    expect(container.textContent).toContain("保险库与加密参数");
    expect(container.textContent).toContain("条目清单");
  });

  it("vault_skeletonIsIdenticalAcrossThreeStates：锁定/未初始化仍保有两枚区块与页脚缺席", async () => {
    for (const state of ["locked", "uninitialized"] as const) {
      await mount(statusOf(state));
      expect(anchorIds().sort(), `${state} 态锚点缺席`).toEqual(["vault.entries", "vault.security"]);
      // 未解锁没有"新建"这回事：工具条与页脚都不出场，但区块骨架不变
      expect(container.querySelectorAll('[role="toolbar"]').length, `${state} 态不该有工具条`).toBe(0);
      expect(container.textContent, `${state} 态条目区应有如实说明`).toContain(
        state === "locked" ? "解锁后这里列出全部凭据条目" : "建库并解锁后，这里列出全部凭据条目",
      );
      act(() => root.unmount());
      container = document.createElement("div");
      document.body.append(container);
    }
  });

  it("vault_toolbarPrimaryCreateIsLastAndFooterCounts：主按钮恒末位＋页脚给全量计数", async () => {
    await mount(statusOf("unlocked"));
    const toolbar = container.querySelector('[role="toolbar"]');
    expect(toolbar).not.toBeNull();
    const labels = [...toolbar!.querySelectorAll("button")].map((b) => b.textContent?.trim());
    expect(labels[labels.length - 1], `00 规范 5 节主按钮恒末位，实得 ${JSON.stringify(labels)}`).toBe(
      "新建条目",
    );

    const footer = container.lastElementChild;
    expect(footer?.textContent).toContain("共 1 条");
    expect(footer?.textContent).toContain("搜索仅按标题，由后端过滤");
  });

  it("vault_kdfHeaderSurfaces：后端已回传的派生参数行内显影（此前整份 DTO 字段无人渲染）", async () => {
    await mount(statusOf("unlocked"));
    const text = container.textContent ?? "";
    expect(text).toContain("argon2id");
    expect(text).toContain("内存 64 MiB");
    expect(text).toContain("迭代 3");
    expect(text).toContain("并行 1");
    // hello_available=false 时免密态必须说"不可用"，不得只显示"未启用"糊弄
    expect(text).toContain("本机 Windows Hello 不可用");
  });

  it("vault_generatorLengthIsFluentSpinButton：原生 number 控件归零且抽屉里给 Fluent 档", async () => {
    await mount(statusOf("unlocked"));
    expect(document.querySelectorAll('input[type="number"]').length).toBe(0);

    await click(buttonByText("新建条目")!);
    // Drawer 与 Dialog 同为 body 门户，按 document 范围查询
    expect(document.body.textContent).toContain("密码生成器");
    expect(document.querySelectorAll('input[type="number"]').length, "抽屉内不得再有浏览器微调钮").toBe(
      0,
    );
    const len = document.querySelector<HTMLInputElement>('input[aria-label="密码长度"]');
    expect(len, "长度控件缺席（SpinButton 未透传 aria-label）").not.toBeNull();
    expect(len!.value).toBe("16");
    expect(len?.getAttribute("role"), "SpinButton 应以 spinbutton 角色申报").toBe("spinbutton");
  });
});
