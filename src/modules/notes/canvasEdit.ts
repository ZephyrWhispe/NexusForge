// T-B7-24 画布纯编辑口：单边删除 / 节点文本改写——与 save 通道解耦，面板只调既有 notesCanvasSave。
import type { CanvasDocDto } from "../../ipc/client";

export function deleteEdge(doc: CanvasDocDto, edgeId: string): CanvasDocDto {
  return { ...doc, edges: doc.edges.filter((e) => e.id !== edgeId) };
}

export function setNodeText(doc: CanvasDocDto, nodeId: string, text: string): CanvasDocDto {
  return { ...doc, nodes: doc.nodes.map((n) => (n.id === nodeId ? { ...n, text } : n)) };
}

/// image 节点 src 归一：已是 URI 则原样，本地绝对路径走 asset 协议
export function canvasImgSrc(src: string, convert: (p: string) => string): string {
  return /^(data:|https?:|asset:|ipc:)/.test(src) ? src : convert(src);
}
