// 由 scripts/gen-pcb.py 生成，勿手改。电流走线（舞台坐标 400x252，起点在外、终点在芯片）
export const PCB_W = 400;
export const PCB_H = 252;
export const PCB_FLOW = [
  {x: -2.0, y: 126.0, len: 128.0, deg: 0.0, delay: 0.0},
  {x: 402.0, y: 126.0, len: 128.0, deg: 180.0, delay: 0.37},
  {x: 200.0, y: -2.0, len: 54.0, deg: 90.0, delay: 0.74},
  {x: 200.0, y: 254.0, len: 54.0, deg: -90.0, delay: 1.11},
  {x: 72.0, y: -2.0, len: 87.7, deg: 45.0, delay: 1.48},
  {x: 328.0, y: -2.0, len: 87.7, deg: 135.0, delay: 0.25},
  {x: 72.0, y: 254.0, len: 87.7, deg: -45.0, delay: 0.62},
  {x: 328.0, y: 254.0, len: 87.7, deg: -135.0, delay: 0.99}
];
