// docs/03 のクラスタ DTO を UI 型へ変換する純粋 mapper。
// fetch の型注釈だけでは runtime のレスポンス乖離を検出できないため、
// UI が依存するクラスタ固有フィールドはここで最小限検証する。

import type {
  Asset,
  Bbox,
  Cluster,
  ClusterAsset,
  Face,
  FaceState,
  SearchResult,
  StackSummary
} from './types';

type JsonRecord = Record<string, unknown>;

const FACE_STATES = new Set<FaceState>([
  'auto',
  'confirmed',
  'candidate',
  'rejected',
  'unassigned'
]);

function record(value: unknown, label: string): JsonRecord {
  if (value == null || typeof value !== 'object' || Array.isArray(value)) {
    throw new TypeError(`invalid ${label} response`);
  }
  return value as JsonRecord;
}

function string(value: unknown, label: string): string {
  if (typeof value !== 'string') throw new TypeError(`invalid ${label} response`);
  return value;
}

function nullableString(value: unknown, label: string): string | null {
  if (value === null) return null;
  return string(value, label);
}

function bbox(value: unknown, label: string): Bbox {
  if (
    !Array.isArray(value) ||
    value.length !== 4 ||
    value.some((coordinate) => typeof coordinate !== 'number' || !Number.isFinite(coordinate))
  ) {
    throw new TypeError(`invalid ${label} response`);
  }
  return value as Bbox;
}

function faceState(value: unknown): FaceState {
  if (typeof value !== 'string' || !FACE_STATES.has(value as FaceState)) {
    throw new TypeError('invalid cluster face state response');
  }
  return value as FaceState;
}

/** `GET /api/clusters` 等の ClusterSummary を UI 表示型へ変換する。 */
export function mapCluster(value: unknown): Cluster {
  const row = record(value, 'cluster');
  const assetCount = row.asset_count;
  if (typeof assetCount !== 'number' || !Number.isSafeInteger(assetCount) || assetCount < 0) {
    throw new TypeError('invalid cluster asset_count response');
  }

  let cover: Face | null = null;
  if (row.cover !== null) {
    const rawCover = record(row.cover, 'cluster cover');
    cover = {
      id: string(rawCover.face_id, 'cluster cover face_id'),
      asset_id: string(rawCover.asset_id, 'cluster cover asset_id'),
      bbox: bbox(rawCover.bbox, 'cluster cover bbox')
    };
  }

  return {
    id: string(row.id, 'cluster id'),
    name: nullableString(row.name, 'cluster name'),
    count: assetCount,
    cover
  };
}

/** クラスタ一覧レスポンスを変換する。 */
export function mapClusters(value: unknown): Cluster[] {
  if (!Array.isArray(value)) throw new TypeError('invalid clusters response');
  return value.map(mapCluster);
}

/**
 * `GET /api/clusters/{id}/assets` の AssetResponse + faces[] を、
 * 1 face = 1 タイルの UI 表示型へ flatten する。
 */
export function mapClusterAssets(value: unknown): ClusterAsset[] {
  if (!Array.isArray(value)) throw new TypeError('invalid cluster assets response');

  return value.flatMap((item) => {
    const row = record(item, 'cluster asset');
    if (!Array.isArray(row.faces)) throw new TypeError('invalid cluster asset faces response');
    const { faces: _faces, ...assetFields } = row;
    const assetId = string(assetFields.id, 'cluster asset id');
    const asset = assetFields as unknown as Asset;

    return row.faces.map((value) => {
      const rawFace = record(value, 'cluster face');
      const similarity = rawFace.similarity;
      if (similarity !== null && typeof similarity !== 'number') {
        throw new TypeError('invalid cluster face similarity response');
      }
      return {
        asset,
        face: {
          id: string(rawFace.face_id, 'cluster face face_id'),
          asset_id: assetId,
          bbox: bbox(rawFace.bbox, 'cluster face bbox'),
          state: faceState(rawFace.state),
          similarity
        }
      } satisfies ClusterAsset;
    });
  });
}

/** 検索結果内の ClusterSummary にも同じ DTO mapper を適用する。 */
export function mapSearchResult(value: unknown): SearchResult {
  const row = record(value, 'search');
  if (!Array.isArray(row.assets) || !Array.isArray(row.stacks) || !Array.isArray(row.clusters)) {
    throw new TypeError('invalid search response');
  }
  return {
    assets: row.assets as Asset[],
    stacks: row.stacks as StackSummary[],
    clusters: mapClusters(row.clusters)
  };
}
