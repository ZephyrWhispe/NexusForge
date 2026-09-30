import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import VaultPanel from "../VaultPanel";
import {
  vaultChangeMasterPassword,
  vaultEntries,
  vaultFolderRename,
  vaultFolders,
  vaultStatus,
  type VaultEntryDto,
  type VaultFolderDto,
} from "../../../ipc/client";
import { notify } from "../../../stores/notifications";

// D-29 B1/T-B1-3 回归：改主密码错误必须内联原样展示 VAULT_UNLOCK_001 且不得宣称
// 锁定保护（该命令不经 unlock()，错旧密不递增计数）；文件夹行内改名真实走
// vault_folder_rename；搜索框查询必须到达后端（null 占位 removal）。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    vaultStatus: vi.fn(),
    vaultFolders: vi.fn(),
    vaultEntries: vi.fn(),
    vaultChangeMasterPassword: vi.fn(),
    vaultFolderRename: vi.fn(),
    vaultFolderCreate: vi.fn(),
    vaultFolderDelete: vi.fn(),
    vaultEntryDelete: vi.fn(),
    vaultEntryAdd: vi.fn(),
    vaultEntryUpdate: vi.fn(),
    vaultCopyPassword: vi.fn(),
    vaultLock: vi.fn(),
    vaultNotifyBlur: vi.fn(),
    vaultTotpNow: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

const unlockedStatus = {
  state: "unlocked" as const,
  lockout_remaining_secs: 0,
  kdf: null,
  hello_enabled: false,
  hello_forced: false,
  hello_available: false,
};

function folder(id: string, name: string): VaultFolderDto {
  return { id, name, created_at: 0 };
}

function entry(id: string, title: string): VaultEntryDto {
  return {
    id,
    folder_id: null,
    title,
    favorite: false,
    fields: [{ key: "user", kind: "text", value: "alice" }],
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

async function setInput(el: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
  await act(async () => {
    setter?.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function mount(search = "") {
  await act(async () => {
    root = createRoot(container);
    root.render(<VaultPanel search={search} group="all" onCounts={() => {}} />);
  });
  await act(async () => {});
  await act(async () => {});
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(vaultStatus).mockResolvedValue(unlockedStatus);
  vi.mocked(vaultFolders).mockResolvedValue([folder("f1", "工作")]);
  vi.mocked(vaultEntries).mockResolvedValue([entry("e1", "GitHub")]);
  vi.mocked(vaultChangeMasterPassword).mockResolvedValue({
    version: 1,
    vault_id: "v1",
    kdf: { algo: "argon2id", m_cost_kib: 65536, t_cost: 3, p_cost: 1, salt_b64: "AA" },
    wrapped_dek: { nonce_b64: "AA", ct_b64: "AA" },
  });
  vi.mocked(vaultFolderRename).mockResolvedValue(true);
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

describe("VaultPanel 改密/夹改名/搜索（T-B1-3）", () => {
  it("vaultChangePassword_wrongOld_showsUnlock001：错旧密内联原样错误且无锁定谎言", async () => {
    vi.mocked(vaultChangeMasterPassword).mockRejectedValue({
      kind: "Module",
      data: { code: "VAULT_UNLOCK_001", message: "解密失败：密码错误或数据被篡改" },
    });
    await mount();
    await click(buttonByText("修改主密码")!);
    // Fluent Dialog 渲染在 body 门户：以 document 范围查询。
    // 三字段按 aria-label 定位（不靠 DOM 序），且必须全部 password 型（永不回显旧密/新密）
    const byLabel = (label: string) => {
      const el = document.querySelector<HTMLInputElement>(`input[aria-label="${label}"]`);
      expect(el, `缺「${label}」输入框`).not.toBeNull();
      return el!;
    };
    expect(document.querySelectorAll('input[type="password"]')).toHaveLength(3);
    for (const label of ["当前主密码", "新主密码", "确认新主密码"])
      expect(byLabel(label).type).toBe("password");
    await setInput(byLabel("当前主密码"), "oldpw");
    await setInput(byLabel("新主密码"), "new-password-9");
    await setInput(byLabel("确认新主密码"), "new-password-9");
    await click(buttonByText("确认修改")!);
    expect(vaultChangeMasterPassword).toHaveBeenCalledWith("oldpw", "new-password-9");
    expect(document.body.textContent).toContain("VAULT_UNLOCK_001: 解密失败：密码错误或数据被篡改");
    // 红线（§4.1 核账⑥）：该路径不递增尝试计数，UI 不得宣称冷却/锁定保护
    expect(document.body.textContent).not.toContain("已锁定");
    expect(document.body.textContent).not.toContain("冷却");
  });

  it("vaultFolderRename_inline_appliesAndRefreshes：铅笔→行内输入→确认走后端并刷新；false 如实示警", async () => {
    await mount();
    await click(container.querySelector('[title="重命名文件夹"]')!);
    // 侧栏 DOM 序：文件夹行在「新文件夹」输入框之上，改名激活后第一个 input 即行内输入
    const input = container.querySelector("input");
    expect(input).not.toBeNull();
    expect(input?.value).toBe("工作");
    await setInput(input!, "个人");
    await click(container.querySelector('[title="确认重命名"]')!);
    expect(vaultFolderRename).toHaveBeenCalledWith("f1", "个人");
    // 成功后 refresh：vaultEntries 至少二次（首轮挂载 + 改名后）
    expect(vi.mocked(vaultEntries).mock.calls.length).toBeGreaterThanOrEqual(2);

    // 负例臂：后端返回 false（id 已不存在）→ warn 文案如实，不谎称已改名
    vi.mocked(vaultFolderRename).mockResolvedValue(false);
    await click(container.querySelector('[title="重命名文件夹"]')!);
    const input2 = container.querySelector<HTMLInputElement>("input");
    await setInput(input2!, "再来一次");
    await click(container.querySelector('[title="确认重命名"]')!);
    expect(notify).toHaveBeenCalledWith("warn", "未找到该文件夹", expect.any(String));
  });

  it("vaultSearch_queryReachesBackend_nullRemoved：检索词以字符串进 vaultEntries，空/空白仍为 null", async () => {
    await mount("git");
    expect(vaultEntries).toHaveBeenCalledWith(null, "git");
    act(() => root.unmount());
    container = document.createElement("div");
    document.body.append(container);

    // 空白查询不参与过滤（null 语义保留，不能把 "" 传下去）
    await mount("   ");
    expect(vaultEntries).toHaveBeenCalledWith(null, null);
  });
});
