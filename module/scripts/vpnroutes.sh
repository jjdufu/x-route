#!/system/bin/sh
# x-route v2: VPN_ROUTES 占位（步骤 08 实现）。被 net.sh 引入。
routes_sync()     { : > "${RUN}/routes.active"; }
routes_reassert() { :; }
routes_clear()    { rm -f "${RUN}/routes.active"; }
