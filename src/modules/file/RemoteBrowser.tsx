import { useCallback, useEffect, useState } from "react";
import {
  Badge,
  Button,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  Input,
  makeStyles,
  Select,
  Table,
  TableBody,
  TableCell,
  TableRow,
  Text,
  tokens,
} from "@fluentui/react-components";
import {
  fileEnqueue,
  fileRemoteBrowse,
  fileRemoteChmod,
  fileRemoteDrivers,
  parseAppError,
  type RemoteDriverDto,
  type RemoteEntryDto,
} from "../../ipc/client";
import { notify, reportError } from "../../stores/notifications";
import EmptyState from "../../components/EmptyState";
import InlineError from "../../components/InlineError";

/**
 * 远端浏览与传输投递（T-B6-10 立浏览面，T-B6-11 接传输臂）：驱动行与根目录
 * **只**出自 file_remote_drivers 的 roots——本地盘符表（fileDrivers）是另一张
 * 脸，混表即把"未连接的远端"假称在场。队列远端执行器已接线（09 §6.2 T-B6-11），
 * 这里的"下载/上传"是把 OpEndpoint 投进 fileEnqueue 的真钮：只入队不搬运，
 * 进度与断点归『传输』档——本面板不自建第二套传输事实源。
 * T-B7-25 添属性弹窗：权限位/符号链接只渲染列表带回的事实源，写回走
 * fileRemoteChmod 唯一口（term 档同一枚命令，此处不做第二份）。
 */

const useStyles = makeStyles({
  toolbar: { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  tableWrap: {
    flex: 1,
    minHeight: 0,
    overflowY: "auto",
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground1,
  },
  nameCell: { maxWidth: "360px", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" },
  row: { cursor: "default" },
});

function fmtSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

export default function RemoteBrowser() {
  const styles = useStyles();
  const [drivers, setDrivers] = useState<RemoteDriverDto[]>([]);
  const [driverId, setDriverId] = useState<string>("");
  const [path, setPath] = useState<string>("");
  const [entries, setEntries] = useState<RemoteEntryDto[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // 传输投递的两侧路径输入：无事实源即拒——不猜 Downloads、不猜默认文件名
  const [dlDst, setDlDst] = useState("");
  const [ulSrc, setUlSrc] = useState("");
  // T-B7-25 权限位面：属性弹窗只认列表带回来的 mode/symlink_target——
  // 弹窗不自造查询第二事实源，None 即不渲染可编辑位（无事实源就无控件）
  const [propsEntry, setPropsEntry] = useState<RemoteEntryDto | null>(null);
  const [propsOctal, setPropsOctal] = useState("");

  const octalOf = (mode: number) => mode.toString(8).padStart(3, "0");

  const applyChmod = async () => {
    if (!propsEntry || !driver) return;
    const text = propsOctal.trim();
    const parsed = /^[0-7]{1,4}$/.test(text) ? Number.parseInt(text, 8) : Number.NaN;
    if (Number.isNaN(parsed)) {
      setError("八进制权限须为 1-4 位 0-7 数字（≤7777）");
      return;
    }
    try {
      await fileRemoteChmod(driver.driver_id, propsEntry.path, parsed);
      notify("success", "权限位已写回", `${propsEntry.name} → 0o${octalOf(parsed)}`);
      setPropsEntry(null);
      void browse(path);
    } catch (e) {
      const ae = parseAppError(e);
      if (ae) setError(`${ae.data.code}: ${ae.data.message}`);
      else {
        reportError(e, { context: "权限位写回异常" });
        setError("权限位写回失败（非典形错误，已上报宿主日志）");
      }
    }
  };

  const enqueueTransfer = async (spec: Parameters<typeof fileEnqueue>[0], what: string) => {
    try {
      const res = await fileEnqueue(spec);
      if (res.op_id) {
        notify("success", `${what}已入队`, "进度与断点见『传输』档");
      } else {
        setError(`未入队${what}：目标有未决议的同名冲突（${res.conflicts.length} 条），先在传输面板决议`);
      }
    } catch (e) {
      const ae = parseAppError(e);
      if (ae) setError(`${ae.data.code}: ${ae.data.message}`);
      else {
        reportError(e, { context: "远端传输入队异常" });
        setError("传输入队失败（非典形错误，已上报宿主日志）");
      }
    }
  };

  const download = (e: RemoteEntryDto) => {
    const dst = dlDst.trim();
    if (!dst) {
      setError("请先填写本地落点目录（不代猜下载目录——无事实源就不承诺）");
      return;
    }
    void enqueueTransfer(
      {
        kind: "copy",
        srcs: [{ driver_id: driver?.driver_id ?? "", path: e.path }],
        dst: `${dst.replace(/[\\/]$/, "")}\\${e.name}`,
        policy: "ask",
      },
      "下载",
    );
  };

  const upload = () => {
    const src = ulSrc.trim();
    if (!src) {
      setError("请先填写本地文件或目录路径（上传不代选源）");
      return;
    }
    const name = src.split(/[\\/]/).filter(Boolean).pop() ?? "";
    const base = path.endsWith("/") ? path.slice(0, -1) : path;
    void enqueueTransfer(
      {
        kind: "copy",
        srcs: [src],
        dst: { driver_id: driver?.driver_id ?? "", path: `${base}/${name}` },
        policy: "ask",
      },
      "上传",
    );
  };

  const refreshDrivers = useCallback(async () => {
    try {
      const list = await fileRemoteDrivers();
      setDrivers(list);
      // 已选驱动被断开后不再在场：选择态跟随事实源收敛，不养幽灵选项
      setDriverId((cur) => (list.some((d) => d.driver_id === cur) ? cur : (list[0]?.driver_id ?? "")));
    } catch (e) {
      const ae = parseAppError(e);
      if (ae) setError(`${ae.data.code}: ${ae.data.message}`);
      else {
        reportError(e, { context: "远端驱动列表异常" });
        setError("远端驱动列表读取失败（非典形错误，已上报宿主日志）");
      }
    }
  }, []);

  useEffect(() => {
    void refreshDrivers();
  }, [refreshDrivers]);

  const driver = drivers.find((d) => d.driver_id === driverId) ?? null;

  const browse = useCallback(
    async (target: string) => {
      if (!driver) return;
      setLoading(true);
      setError(null);
      setPath(target);
      try {
        setEntries(await fileRemoteBrowse(driver.driver_id, target));
      } catch (e) {
        setEntries(null);
        const ae = parseAppError(e);
        if (ae) setError(`${ae.data.code}: ${ae.data.message}`);
        else {
          reportError(e, { context: "远端目录浏览异常" });
          setError("远端目录读取失败（非典形错误，已上报宿主日志）");
        }
      } finally {
        setLoading(false);
      }
    },
    [driver],
  );

  // 换驱动即清列表：上一驱动的条目留在屏上就是对另一驱动撒谎
  useEffect(() => {
    setEntries(null);
    setPath(driver ? driver.roots[0] ?? "" : "");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [driverId]);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: "8px", minHeight: 0, flex: 1 }}>
      <div className={styles.toolbar}>
        <Text weight="semibold" size={300}>
          远端浏览
        </Text>
        <Select
          size="small"
          aria-label="远端驱动"
          value={driverId}
          onChange={(_, d) => setDriverId(d.value)}
          style={{ minWidth: "180px" }}
        >
          {drivers.length === 0 && <option value="">（尚无已连接远端）</option>}
          {drivers.map((d) => (
            <option key={d.driver_id} value={d.driver_id}>
              {d.label}
            </option>
          ))}
        </Select>
        {driver && (
          <Select
            size="small"
            aria-label="根目录"
            value={driver.roots.includes(path) ? path : driver.roots[0] ?? ""}
            onChange={(_, d) => void browse(d.value)}
            style={{ minWidth: "120px" }}
          >
            {/* 根目录只出自 file_remote_drivers.roots（承重⑮）：本地盘符禁入本表 */}
            {driver.roots.map((r) => (
              <option key={r} value={r}>
                {r}
              </option>
            ))}
          </Select>
        )}
        <Input
          size="small"
          aria-label="远端路径"
          value={path}
          onChange={(_, d) => setPath(d.value)}
          onKeyDown={(ev) => ev.key === "Enter" && path && void browse(path)}
          style={{ maxWidth: "240px" }}
        />
        <Button size="small" disabled={!driver || loading} onClick={() => void browse(path)}>
          浏览
        </Button>
        <Button size="small" appearance="subtle" onClick={() => void refreshDrivers()}>
          刷新
        </Button>
        {driver && <Badge appearance="outline">凭据来源 {driver.auth_source}</Badge>}
      </div>
      <div className={styles.toolbar}>
        {/* 传输投递（T-B6-11）：只入队，进度/断点归『传输』档——本行不建第二套事实源 */}
        <Input
          size="small"
          aria-label="本地落点目录"
          placeholder="下载落点：本地目录路径"
          value={dlDst}
          onChange={(_, d) => setDlDst(d.value)}
          style={{ maxWidth: "220px" }}
        />
        <Input
          size="small"
          aria-label="本地上传源"
          placeholder="上传源：本地文件或目录路径"
          value={ulSrc}
          onChange={(_, d) => setUlSrc(d.value)}
          style={{ maxWidth: "220px" }}
        />
        <Button size="small" disabled={!driver || !ulSrc.trim()} onClick={() => upload()}>
          上传到此目录
        </Button>
      </div>
      {loading && (
        <div className={styles.toolbar} role="status">
          <Text className={styles.muted}>远端目录加载中…</Text>
        </div>
      )}
      {error && <InlineError text={error} />}
      {!driver && !loading && (
        <EmptyState text="尚无已连接远端：先在上方『远程连接』完成连接，这里才会列出驱动" />
      )}
      {driver && entries && (
        <div className={styles.tableWrap}>
          <Table>
            <TableBody>
              {entries.map((e) => (
                <TableRow
                  key={e.path}
                  className={styles.row}
                  onDoubleClick={() => e.is_dir && void browse(e.path)}
                >
                  <TableCell className={styles.nameCell}>
                    {e.is_dir ? "📁 " : "📄 "}
                    {e.name}
                  </TableCell>
                  <TableCell>
                    <Text className={styles.muted}>{e.is_dir ? "目录" : fmtSize(e.size)}</Text>
                  </TableCell>
                  <TableCell>
                    {/* 目录下载随远端目录树的队列展开臂在场（fileEnqueue 接受目录源，
                        执行器走 walk_remote）；钮对目录同样给——语义由后端裁决非本面猜 */}
                    <Button size="small" appearance="subtle" onClick={() => download(e)}>
                      下载
                    </Button>
                    <Button
                      size="small"
                      appearance="subtle"
                      data-properties
                      onClick={() => {
                        setPropsEntry(e);
                        setPropsOctal(e.mode != null ? octalOf(e.mode) : "");
                      }}
                    >
                      属性
                    </Button>
                  </TableCell>
                </TableRow>
              ))}
              {entries.length === 0 && (
                <TableRow>
                  <TableCell>
                    <Text className={styles.muted}>该远端目录为空</Text>
                  </TableCell>
                </TableRow>
              )}
            </TableBody>
          </Table>
        </div>
      )}
      {propsEntry && (
        <Dialog open onOpenChange={(_, d) => !d.open && setPropsEntry(null)}>
          <DialogSurface>
            <DialogBody>
              <DialogTitle>属性：{propsEntry.name}</DialogTitle>
              <DialogContent>
                <div style={{ display: "flex", flexDirection: "column", gap: "6px" }}>
                  <Text size={200}>路径 {propsEntry.path}</Text>
                  {propsEntry.mode != null && (
                    <Text size={200}>权限位（八进制）0o{octalOf(propsEntry.mode)}</Text>
                  )}
                  {propsEntry.symlink_target != null && (
                    <Text size={200}>符号链接目标 {propsEntry.symlink_target}</Text>
                  )}
                  {propsEntry.mode == null && (
                    <Text className={styles.muted}>
                      该条目无权限位事实源（列目录不带回 mode 的协议即不渲染可编辑位）
                    </Text>
                  )}
                  {propsEntry.mode != null && (
                    <div className={styles.toolbar}>
                      <Input
                        id="remote-props-octal"
                        size="small"
                        aria-label="新八进制权限位"
                        value={propsOctal}
                        onChange={(_, d) => setPropsOctal(d.value)}
                        style={{ maxWidth: "120px" }}
                      />
                      <Button size="small" onClick={() => void applyChmod()}>
                        写回
                      </Button>
                    </div>
                  )}
                </div>
              </DialogContent>
              <DialogActions>
                <Button appearance="subtle" onClick={() => setPropsEntry(null)}>
                  关闭
                </Button>
              </DialogActions>
            </DialogBody>
          </DialogSurface>
        </Dialog>
      )}
    </div>
  );
}
