// 由 scripts/gen-pcb.py 生成，勿手改。电流走线（舞台坐标 480x320，起点在外、终点在芯片）
export const PCB_W = 480;
export const PCB_H = 320;
export const PCB_FLOW = [
  {x: -2.0, y: 160.0, len: 168.0, deg: 0.0, delay: 0.0},
  {x: 482.0, y: 160.0, len: 168.0, deg: 180.0, delay: 0.37},
  {x: 240.0, y: -2.0, len: 88.0, deg: 90.0, delay: 0.74},
  {x: 240.0, y: 322.0, len: 88.0, deg: -90.0, delay: 1.11},
  {x: 78.0, y: -2.0, len: 135.8, deg: 45.0, delay: 1.48},
  {x: 402.0, y: -2.0, len: 135.8, deg: 135.0, delay: 0.25},
  {x: 78.0, y: 322.0, len: 135.8, deg: -45.0, delay: 0.62},
  {x: 402.0, y: 322.0, len: 135.8, deg: -135.0, delay: 0.99}
];
