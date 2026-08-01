import { describe, expect, it } from 'vitest';

import { mapCluster, mapClusterAssets, mapSearchResult } from './mappers';

const asset = {
  id: 'asset-1',
  filename: 'sample.png',
  width: 1200,
  height: 800,
  ratio: 1.5,
  thumbhash: null,
  taken_at: '2026-08-02T00:00:00Z',
  created_at: '2026-08-02T00:00:00Z',
  status: 'created'
};

describe('cluster DTO mappers', () => {
  it('docs/03 の代表顔付き ClusterSummary を UI 型へ変換する', () => {
    expect(
      mapCluster({
        id: 'cluster-1',
        name: '主人公',
        cover: { face_id: 'face-cover', asset_id: 'asset-1', bbox: [0.1, 0.2, 0.3, 0.4] },
        asset_count: 2
      })
    ).toEqual({
      id: 'cluster-1',
      name: '主人公',
      cover: {
        id: 'face-cover',
        asset_id: 'asset-1',
        bbox: [0.1, 0.2, 0.3, 0.4]
      },
      count: 2
    });
  });

  it('同一 asset の faces[] 全件を face_id 単位のタイルへ flatten する', () => {
    const mapped = mapClusterAssets([
      {
        ...asset,
        faces: [
          {
            face_id: 'face-a',
            bbox: [0.1, 0.1, 0.2, 0.2],
            state: 'auto',
            similarity: 0.91
          },
          {
            face_id: 'face-b',
            bbox: [0.6, 0.2, 0.25, 0.3],
            state: 'confirmed',
            similarity: null
          }
        ]
      }
    ]);

    expect(mapped.map((item) => item.face.id)).toEqual(['face-a', 'face-b']);
    expect(mapped.map((item) => item.face.asset_id)).toEqual(['asset-1', 'asset-1']);
    expect(mapped[0].asset).not.toHaveProperty('faces');
    expect(mapped[1].face.state).toBe('confirmed');
  });

  it('検索結果内の clusters にも ClusterSummary mapper を適用する', () => {
    const result = mapSearchResult({
      assets: [asset],
      stacks: [],
      clusters: [{ id: 'cluster-1', name: null, cover: null, asset_count: 1 }]
    });

    expect(result.assets).toEqual([asset]);
    expect(result.clusters).toEqual([{ id: 'cluster-1', name: null, cover: null, count: 1 }]);
  });

  it('旧 cover_face_id DTO や faces 欠落を受理しない', () => {
    expect(() =>
      mapCluster({ id: 'cluster-1', name: null, cover_face_id: 'face-1', asset_count: 1 })
    ).toThrow('invalid cluster cover response');
    expect(() => mapClusterAssets([asset])).toThrow('invalid cluster asset faces response');
  });
});
