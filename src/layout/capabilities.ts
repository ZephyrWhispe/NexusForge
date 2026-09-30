import { PANELS } from "./panels";
import type { ModuleId } from "./modules";

/**
 * 面板能力清单（D-43 ③）：把"这个页面能做什么、能做到什么程度"写进界面本身。
 *
 * 病因是实测出来的：全仓 grep 命令面板/快捷键总览/使用帮助/onboarding 零命中，而唯一
 * 承载能力句的 PanelHeader context 槽被 nowrap+ellipsis 裁成半句（D-42 已追加更正）。
 * 这里只做事实登记，不做宣传——四条判据：
 *  ① `collapsed` 一律取 PANELS 的 subtitle（单一真源，杜绝两处文案漂移）；
 *  ② `can` 只写面板里真有 IPC 调用站点或真渲出的能力；
 *  ③ `limits` 只写源码里数得出的常数上限（截断/缓冲/冷却），一条不落地把静默截断显影；
 *  ④ `notYet` 只写已在 DECISIONS/面板档登记为未交付的项，不写"将来也许有"。
 * 十枚未重排面板只给 collapsed（无 detail ⇒ 卡片不可展开），避免用未经核读的清单装完成。
 */

export type CapabilityDetail = {
  can: string[];
  notYet: string[];
  limits: string[];
  keys: string[];
};

export type ModuleCapabilities = { collapsed: string; detail?: CapabilityDetail };

const DETAIL: Partial<Record<ModuleId, CapabilityDetail>> = {
  vault: {
    can: [
      "主密码派生密钥并加解锁",
      "条目增删改与文件夹分组",
      "密码生成器与口令复制到剪贴板",
      "TOTP 取码与倒计时",
      "免密（Hello）开关与失败熔断",
      "修改主密码与自动锁定预警",
    ],
    notYet: ["口令健康报告", "条目版本历史与回滚", "导入导出与附件"],
    limits: [
      "主密码不少于 8 位",
      "连续错误 5 次锁 300 秒",
      "免密连续失败 5 次自动熔断",
      "生成器长度下限 4、默认 16",
      "搜索只按标题匹配（后端执行）",
      "自动锁定预警默认 30 秒",
    ],
    keys: ["回车：解锁与确认", "Esc：取消重命名", "回车：新建文件夹"],
  },
  term: {
    can: [
      "本地 ConPTY 终端",
      "WSL 分发会话",
      "SSH 连接（首连指纹确认）",
      "SFTP 浏览/上传/下载/改权限",
      "端口转发（本地 / 远端 / SOCKS5）",
      "一次性远端命令（无 PTY）",
      "Docker 容器日志与启停",
      "已知主机指纹管理",
    ],
    notYet: ["SSH 连接池统一", "命令片段库与会话档案持久化"],
    limits: [
      "会话尺寸固定 100×26",
      "一次性命令区 80×24",
      "Docker 日志只取末 200 行",
      "权限位八进制不超过 7777",
      "转发端口 1–65535",
      "跳板只构造单跳（多跳未实现）",
    ],
    keys: ["回车/空格：打开与关闭会话页签"],
  },
  sys: {
    can: [
      "CPU/内存/磁盘秒级采样曲线",
      "进程列表检索与终止",
      "清理扫描（白名单）与执行",
      "winget/scoop/choco 清单、搜索与命令预览",
      "系统调整项扫描/应用/回滚",
      "调整审计导出与分类目录",
    ],
    notYet: ["启动项管理", "磁盘占用 treemap", "软件源增删", "更新管控"],
    limits: [
      "指标曲线保留最近 300 点",
      "包管理输出缓冲 200 行、只渲染末 30 行",
      "已装清单只展示前 200 条",
      "搜索结果只展示前 100 条",
      "24 小时内修改的文件跳过清理",
      "内置清理目标 4 项",
      "未扫描时执行按钮不可用",
    ],
    keys: ["回车：进程搜索", "回车：在线搜索"],
  },
  notes: {
    can: [
      "Markdown 笔记读写",
      "[[双链]] 出链与反链",
      "标签索引与筛选",
      "SM-2 复习队列与四档评分",
      "自由画布（节点与连线保存）",
      "编辑 / 预览 / 大纲三栏",
      "笔记库局域网同步",
    ],
    notYet: ["标签治理（合并与重命名）", "每日笔记", "全局图谱视图"],
    limits: [
      "搜索结果上限 200 条",
      "每行标签最多显示 3 枚",
      "画布引用笔记下拉前 50 条",
      "评分档固定 4 档",
      "搜索防抖 200 毫秒",
    ],
    keys: ["Ctrl+S：保存", "Delete：删除选中连线", "[[：触发双链补全"],
  },
};

/** 能力卡取数：collapsed 恒有（PANELS 的 subtitle 为单一真源），detail 仅已核读的四枚 */
export function capabilitiesFor(id: ModuleId): ModuleCapabilities {
  const detail = DETAIL[id];
  return detail ? { collapsed: PANELS[id].subtitle, detail } : { collapsed: PANELS[id].subtitle };
}
