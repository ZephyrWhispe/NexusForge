import {
  makeStyles,
  tokens,
  Button,
  Input,
  Switch,
  Table,
  TableBody,
  TableCell,
  TableRow,
  RadioGroup,
  Radio,
} from "@fluentui/react-components";
import type { ProxyRuleV2Dto, ProxyRulesV2Dto } from "../../../ipc/client";
import { confirmAction } from "../../../stores/confirm";
import Section from "../../../components/Section";

/**
 * 分流子面板（T-B2-9：直连清单升级三目标规则表 v2）。
 * 编辑只改本地下钻（onChange 冒泡给根置脏位），「保存规则」才全量 invoke
 * proxy_rules_set——后端 sanitize 校验闸违规点名值回显 InlineError。
 * 行内下拉用原生 select（automation/RulesPanel 同款先例：表格编辑器 jsdom 可测）。
 * 缺陷⑪a：示例 placeholder 的反斜杠转义修正（`\b` 两字符字面，见 PRESET 区 Input）。
 */

/** 「应用大陆直连预设」清单（与后端 rules.rs MAINLAND_DIRECT_PRESET 同源；geo 资产通道已随 T-B2-10 落地，本预设保持零资产依赖） */
export const MAINLAND_DIRECT_PRESET: string[] = [
  "cn",
  "com.cn",
  "net.cn",
  "org.cn",
  "gov.cn",
  "edu.cn",
  "baidu.com",
  "qq.com",
  "weixin.qq.com",
  "163.com",
  "126.com",
  "bilibili.com",
  "taobao.com",
  "tmall.com",
  "jd.com",
  "douyin.com",
  "toutiao.com",
  "zhihu.com",
  "xiaohongshu.com",
  "meituan.com",
];

const KIND_LABELS: Record<ProxyRulesV2Dto["rules"][number]["kind"], string> = {
  domain: "域名（精确）",
  suffix: "域名后缀",
  keyword: "域名关键字",
  ip_cidr: "IP 网段",
  process: "进程名",
  geo_site: "GeoSite 码表",
  geo_ip: "GeoIP 码表",
};

const TARGET_LABELS: Record<ProxyRulesV2Dto["rules"][number]["target"], string> = {
  direct: "直连",
  proxy: "代理",
  block: "拦截",
};

const useStyles = makeStyles({
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  row: { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" },
  select: {
    fontSize: tokens.fontSizeBase200,
    padding: "4px 6px",
    borderRadius: tokens.borderRadiusMedium,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    background: tokens.colorNeutralBackground1,
    color: tokens.colorNeutralForeground1,
  },
});

export default function RulesSection({
  value,
  loaded,
  busy,
  onChange,
  onSave,
  onApplyPreset,
}: {
  value: ProxyRulesV2Dto | null;
  loaded: boolean;
  busy: string;
  onChange: (next: ProxyRulesV2Dto) => void;
  onSave: () => void;
  onApplyPreset: (next: ProxyRulesV2Dto) => void;
}) {
  const styles = useStyles();
  if (!value) {
    return (
      <Section title="分流规则">
        <span className={styles.muted}>{loaded ? "规则表加载失败" : "规则表加载中…"}</span>
      </Section>
    );
  }
  const v2 = value;
  const patch = (i: number, p: Partial<ProxyRuleV2Dto>) => {
    const rules = v2.rules.map((r, k) => (k === i ? { ...r, ...p } : r));
    onChange({ ...v2, rules });
  };
  const remove = (i: number) => {
    onChange({ ...v2, rules: v2.rules.filter((_, k) => k !== i) });
  };
  const addRow = () => {
    onChange({
      ...v2,
      rules: [
        ...v2.rules,
        { kind: "suffix", pattern: "", target: "direct", enabled: true },
      ],
    });
  };
  const applyPreset = async () => {
    // 去重合并：同 (kind=suffix, pattern, target=direct) 已在场即跳过（含禁用行）
    const fresh = MAINLAND_DIRECT_PRESET.filter(
      (p) =>
        !v2.rules.some((r) => r.kind === "suffix" && r.target === "direct" && r.pattern === p),
    );
    const skipped = MAINLAND_DIRECT_PRESET.length - fresh.length;
    if (fresh.length === 0) return;
    if (
      !(await confirmAction({
        title: "应用大陆直连预设",
        impact: [
          `将新增 ${fresh.length} 条「域名后缀 → 直连」规则`,
          `已存在的 ${skipped} 条自动跳过（去重合并，不产生重复行）`,
        ],
        detail:
          "geo 码表规则（GeoSite/GeoIP）需先在「内核」子面板安装对应数据资产；本预设是纯域名后缀清单，无资产依赖。应用即保存，需重新切换模式生效。",
        confirmLabel: "应用",
      }))
    )
      return;
    onApplyPreset({
      ...v2,
      rules: [
        ...v2.rules,
        ...fresh.map<ProxyRuleV2Dto>((pattern) => ({
          kind: "suffix",
          pattern,
          target: "direct",
          enabled: true,
        })),
      ],
    });
  };
  return (
    <Section
      title="分流规则"
      actions={
        <span className={styles.muted}>
          {/* D-42：直连后缀生效数是后端 proxy_direct_rules 的同一口径（service.rs:704 就是
              对本表 kind=suffix∧target=direct∧enabled 的投影，无独立事实源），就地汇总即可，
              不必为一条派生数多开一路 IPC；表改未保存时这里的数跟着本地走 */}
          直连后缀生效 {v2.rules.filter((r) => r.kind === "suffix" && r.target === "direct" && r.enabled).length} 条
          · 命中即按目标走（直连/代理/拦截）；改完保存并重新切换模式后生效
        </span>
      }
    >
      <RadioGroup
        layout="horizontal"
        aria-label="分流模式"
        value={v2.route_mode}
        onChange={(_, d) =>
          onChange({ ...v2, route_mode: d.value as ProxyRulesV2Dto["route_mode"] })
        }
      >
        <Radio value="global" label="全局代理（规则全跳过）" />
        <Radio value="rule" label="规则分流" />
        <Radio value="direct_all" label="全局直连（透明兜底档）" />
      </RadioGroup>
      <Table size="small">
        <TableBody>
          {v2.rules.map((r, i) => (
            <TableRow key={i}>
              <TableCell>
                <Switch
                  checked={r.enabled}
                  onChange={(_, d) => patch(i, { enabled: d.checked })}
                  aria-label={`启用规则 ${i + 1}`}
                />
              </TableCell>
              <TableCell>
                <select
                  className={styles.select}
                  aria-label={`规则类型 ${i + 1}`}
                  value={r.kind}
                  onChange={(e) => patch(i, { kind: e.target.value as ProxyRuleV2Dto["kind"] })}
                >
                  {(Object.keys(KIND_LABELS) as (keyof typeof KIND_LABELS)[]).map((k) => (
                    <option key={k} value={k}>
                      {KIND_LABELS[k]}
                    </option>
                  ))}
                </select>
              </TableCell>
              <TableCell>
                <Input
                  size="small"
                  value={r.pattern}
                  onChange={(_, d) => patch(i, { pattern: d.value })}
                  aria-label={`规则值 ${i + 1}`}
                  /* 缺陷⑪a 修正：示例反斜杠以 `\\b` 字面两字符呈现（旧串 `\b` 是退格控制符） */
                  placeholder={"cn\\baidu.com · 10.0.0.0/8 · chrome.exe · cn/category-ads（geo 码）"}
                />
              </TableCell>
              <TableCell>
                <select
                  className={styles.select}
                  aria-label={`规则目标 ${i + 1}`}
                  value={r.target}
                  onChange={(e) =>
                    patch(i, { target: e.target.value as ProxyRuleV2Dto["target"] })
                  }
                >
                  {(Object.keys(TARGET_LABELS) as (keyof typeof TARGET_LABELS)[]).map((k) => (
                    <option key={k} value={k}>
                      {TARGET_LABELS[k]}
                    </option>
                  ))}
                </select>
              </TableCell>
              <TableCell>
                <Button size="small" appearance="subtle" onClick={() => remove(i)}>
                  删
                </Button>
              </TableCell>
            </TableRow>
          ))}
          {/* 表尾固定兜底行：不可删（无删除钮），只换目标；route_mode=rule 时生效 */}
          <TableRow>
            <TableCell>
              <span className={styles.muted}>兜底</span>
            </TableCell>
            <TableCell>
              <span className={styles.muted}>未匹配流量</span>
            </TableCell>
            <TableCell colSpan={2}>
              <select
                className={styles.select}
                aria-label="兜底目标"
                value={v2.final_target}
                onChange={(e) =>
                  onChange({
                    ...v2,
                    final_target: e.target.value as ProxyRulesV2Dto["final_target"],
                  })
                }
              >
                <option value="proxy">代理</option>
                <option value="direct">直连</option>
                <option value="block">拦截</option>
              </select>
            </TableCell>
            <TableCell />
          </TableRow>
        </TableBody>
      </Table>
      <div className={styles.row}>
        <Button size="small" disabled={busy !== ""} onClick={addRow}>
          新增规则
        </Button>
        <Button size="small" disabled={busy !== ""} onClick={applyPreset}>
          应用大陆直连预设
        </Button>
        <Button size="small" disabled={busy !== ""} onClick={onSave}>
          {busy === "rules" ? "保存中…" : "保存规则"}
        </Button>
      </div>
      <span className={styles.muted}>
        进程名规则仅 TUN 模式可归因命中（系统代理模式下内核看不到进程）；
        校验在后端收口：IP 网段/域名形态/进程名非法会点名拒写。
      </span>
    </Section>
  );
}
