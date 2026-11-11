package com.xrouter.keepalive;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.os.Build;
import android.os.IBinder;

/**
 * 常驻通知 + 前台服务（保活的保险，真正的存活靠 root 守护进程）。无界面。
 * 守护进程约每 10 秒经 am start-foreground-service 推送累计流量（息屏暂停、不变不推）：
 *   --es state 运行中|已就绪|启动中  --es mode total|wan|lan
 *   --el wan_up/wan_dn/lan_up/lan_dn 字节数
 * 收起时按 mode 显示一行，展开后显示公网、内网两行。点击打开模块 WebUI。
 */
public class KeepAliveService extends Service {
    public static final String CHANNEL_ID = "xrouter_status";
    private static final String OLD_CHANNEL_ID = "xrouter";
    public static final int NOTIFY_ID = 233;

    private String state = "运行中";
    private String mode = "total";
    private long wanUp, wanDn, lanUp, lanDn;
    private boolean hasTraffic;

    @Override
    public void onCreate() {
        super.onCreate();
        NotificationManager nm = (NotificationManager) getSystemService(NOTIFICATION_SERVICE);
        if (nm != null) {
            // 默认重要性：状态栏有图标、通知栏可见；关掉声音与振动，不弹横幅
            NotificationChannel ch = new NotificationChannel(
                    CHANNEL_ID, "XRouter 运行状态", NotificationManager.IMPORTANCE_DEFAULT);
            ch.setDescription("虚拟机运行状态与累计流量");
            ch.setShowBadge(false);
            ch.setSound(null, null);
            ch.enableVibration(false);
            ch.enableLights(false);
            ch.setLockscreenVisibility(Notification.VISIBILITY_PUBLIC);
            nm.createNotificationChannel(ch);
            nm.deleteNotificationChannel(OLD_CHANNEL_ID);
        }
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        if (intent != null) {
            String s = intent.getStringExtra("state");
            if (s == null) s = intent.getStringExtra("text");
            if (s != null && !s.isEmpty()) state = s;
            String m = intent.getStringExtra("mode");
            if ("total".equals(m) || "wan".equals(m) || "lan".equals(m)) mode = m;
            if (intent.hasExtra("wan_up")) {
                wanUp = intent.getLongExtra("wan_up", 0);
                wanDn = intent.getLongExtra("wan_dn", 0);
                lanUp = intent.getLongExtra("lan_up", 0);
                lanDn = intent.getLongExtra("lan_dn", 0);
                hasTraffic = true;
            }
        }
        Notification n = build();
        if (Build.VERSION.SDK_INT >= 34) {
            startForeground(NOTIFY_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE);
        } else {
            startForeground(NOTIFY_ID, n);
        }
        return START_STICKY;
    }

    private Notification build() {
        Intent open = new Intent(this, OpenActivity.class)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_NO_ANIMATION);
        PendingIntent pi = PendingIntent.getActivity(this, 0, open,
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        Notification.Builder b = new Notification.Builder(this, CHANNEL_ID)
                .setContentTitle("XRouter · " + state)
                .setSmallIcon(R.drawable.ic_stat)
                .setContentIntent(pi)
                .setOngoing(true)
                .setOnlyAlertOnce(true)
                .setShowWhen(false)
                .setCategory(Notification.CATEGORY_SERVICE)
                .setVisibility(Notification.VISIBILITY_PUBLIC);
        if (Build.VERSION.SDK_INT >= 31) {
            b.setForegroundServiceBehavior(Notification.FOREGROUND_SERVICE_IMMEDIATE);
        }
        if (hasTraffic) {
            b.setContentText(Traffic.collapsed(mode, wanUp, wanDn, lanUp, lanDn));
            b.setStyle(new Notification.BigTextStyle()
                    .bigText(Traffic.expanded(wanUp, wanDn, lanUp, lanDn)));
        } else {
            b.setContentText(state);
        }
        return b.build();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
