package com.xrouter.notify;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.Paint;
import android.graphics.RectF;
import android.graphics.drawable.Icon;
import android.net.Uri;
import android.os.IBinder;

import java.lang.reflect.Method;
import java.util.Collections;
import java.util.List;

/**
 * 以 "android" 包身份直接向 NotificationManagerService 发常驻通知，不安装任何 App。
 * 做法与 LSPosed 守护进程 lspd 相同（LSPNotificationManager）：
 *   ServiceManager.getService("notification") -> INotificationManager，
 *   createNotificationChannelsForPackage("android", 1000, ...) 建渠道，
 *   enqueueNotificationWithTag("android", "android", tag, id, n, 0) 发通知，
 *   cancelNotificationWithTag("android", "android", tag, id, 0) 撤通知。
 * root（uid 0）调用时 NMS 视为系统调用方，通知归属 android（uid 1000）。
 * 隐藏接口一律反射调用：app_process 起的进程不受应用的隐藏 API 限制（lspd、scrcpy 同理）。
 */
final class Notifier {
    static final String PKG = "android";
    static final int SYSTEM_UID = 1000;
    /** v3.6.0 起换新渠道：渠道的锁屏可见性只在创建时生效，旧渠道（锁屏公开）删除后按新设置重建。 */
    static final String CHANNEL_ID = "xrouter_status_v2";
    static final String OLD_CHANNEL_ID = "xrouter_status";
    static final String TAG = "xrouter";
    static final int ID = 233;
    static final String MODULE_ID = "xrouter";
    static final String[] MANAGERS = {"com.sukisu.ultra", "me.weishu.kernelsu", "com.rifsxd.ksunext"};

    private final Context ctx;
    private IBinder binder;
    private Object nm;
    private boolean channelReady;
    private PendingIntent tap;
    private boolean tapResolved;
    private Icon icon;

    Notifier(Context ctx) {
        this.ctx = ctx;
    }

    /** system_server 重启后旧 binder 失效，重新取。 */
    private Object nm() throws Exception {
        if (nm == null || binder == null || !binder.isBinderAlive()) {
            Class<?> sm = Class.forName("android.os.ServiceManager");
            IBinder b = (IBinder) sm.getMethod("getService", String.class).invoke(null, Context.NOTIFICATION_SERVICE);
            if (b == null) throw new IllegalStateException("notification 服务不可用");
            Class<?> stub = Class.forName("android.app.INotificationManager$Stub");
            nm = stub.getMethod("asInterface", IBinder.class).invoke(null, b);
            binder = b;
            channelReady = false;
        }
        return nm;
    }

    /** 按名称与参数个数找 INotificationManager 的方法；ROM 改了签名时把实际签名写进错误里便于排查。 */
    private static Object call(Object target, String name, Object... args) throws Exception {
        for (Method m : target.getClass().getMethods()) {
            if (m.getName().equals(name) && m.getParameterTypes().length == args.length) {
                return m.invoke(target, args);
            }
        }
        StringBuilder sb = new StringBuilder("找不到 ").append(name).append('/').append(args.length).append("，现有：");
        for (Method m : target.getClass().getMethods()) {
            if (m.getName().equals(name)) sb.append(m.toGenericString()).append("; ");
        }
        throw new NoSuchMethodException(sb.toString());
    }

    private void ensureChannel() throws Exception {
        if (channelReady) return;
        // 默认重要性：状态栏有图标、通知栏可见；关掉声音、振动与呼吸灯，不弹横幅。
        // 只创建不更新：已存在时不覆盖用户在系统设置里对该渠道的修改。
        NotificationChannel ch = new NotificationChannel(CHANNEL_ID, "XRouter 运行状态",
                NotificationManager.IMPORTANCE_DEFAULT);
        ch.setDescription("XRouter 运行状态与累计流量");
        ch.setShowBadge(false);
        ch.setSound(null, null);
        ch.enableVibration(false);
        ch.enableLights(false);
        // 只在通知栏常驻，不上锁屏（不影响常驻保活本身）
        ch.setLockscreenVisibility(Notification.VISIBILITY_SECRET);
        try {
            call(nm(), "deleteNotificationChannel", PKG, OLD_CHANNEL_ID);
        } catch (Throwable ignored) {
            // 旧渠道不存在或 ROM 不允许删除：不影响新渠道
        }
        Class<?> pls = Class.forName("android.content.pm.ParceledListSlice");
        Object list = pls.getConstructor(List.class).newInstance(Collections.singletonList(ch));
        call(nm(), "createNotificationChannelsForPackage", PKG, SYSTEM_UID, list);
        channelReady = true;
    }

    /** 点击：优先直接打开管理器的 WebUIActivity（未导出，但 root 创建的 PendingIntent 可以启动）。 */
    private PendingIntent tap() {
        if (tapResolved) return tap;
        tapResolved = true;
        Intent i = resolveTapIntent();
        if (i == null) {
            Main.log("未找到 KernelSU / SukiSU 管理器，通知不带点击动作");
            return null;
        }
        try {
            tap = PendingIntent.getActivity(ctx, 0, i,
                    PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
            Main.log("点击目标：" + i);
        } catch (Throwable t) {
            Main.log("创建点击动作失败：" + t);
            tap = null;
        }
        return tap;
    }

    private Intent resolveTapIntent() {
        PackageManager pm = ctx.getPackageManager();
        Uri uri = Uri.parse("ksu://webui?id=" + MODULE_ID);
        for (String pkg : MANAGERS) {
            try {
                pm.getPackageInfo(pkg, 0);
            } catch (Throwable t) {
                continue;
            }
            String[] classes = {pkg + ".ui.webui.WebUIActivity", "me.weishu.kernelsu.ui.webui.WebUIActivity"};
            for (String cls : classes) {
                ComponentName cn = new ComponentName(pkg, cls);
                try {
                    pm.getActivityInfo(cn, 0);
                } catch (Throwable t) {
                    continue;
                }
                return new Intent(Intent.ACTION_VIEW, uri).setComponent(cn)
                        .putExtra("id", MODULE_ID).putExtra("name", "XRouter")
                        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            }
            // 管理器主界面导出了 ksu://webui 入口
            Intent view = new Intent(Intent.ACTION_VIEW, uri).setPackage(pkg)
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            try {
                if (!pm.queryIntentActivities(view, 0).isEmpty()) return view;
            } catch (Throwable ignored) {
                // 继续回退
            }
            Intent launch = pm.getLaunchIntentForPackage(pkg);
            if (launch != null) return launch.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        }
        return null;
    }

    /** 没有资源文件可用：画一个白色单色路由器图标（状态栏只取 alpha）。 */
    private Icon icon() {
        if (icon != null) return icon;
        try {
            int s = 96;
            Bitmap bmp = Bitmap.createBitmap(s, s, Bitmap.Config.ARGB_8888);
            Canvas c = new Canvas(bmp);
            Paint p = new Paint(Paint.ANTI_ALIAS_FLAG);
            p.setColor(0xFFFFFFFF);
            c.drawRoundRect(new RectF(8, 52, 88, 84), 10, 10, p);
            p.setStrokeWidth(7);
            p.setStrokeCap(Paint.Cap.ROUND);
            c.drawLine(28, 52, 18, 14, p);
            c.drawLine(68, 52, 78, 14, p);
            p.setColor(0x00000000);
            p.setXfermode(new android.graphics.PorterDuffXfermode(android.graphics.PorterDuff.Mode.CLEAR));
            c.drawCircle(24, 68, 5, p);
            c.drawCircle(40, 68, 5, p);
            icon = Icon.createWithBitmap(bmp);
        } catch (Throwable t) {
            Main.log("绘制图标失败，改用系统图标：" + t);
            icon = Icon.createWithResource(PKG, android.R.drawable.stat_sys_warning);
        }
        return icon;
    }

    Notification build(Traffic.Payload p) {
        Notification.Builder b = new Notification.Builder(ctx, CHANNEL_ID)
                .setContentTitle(p.title())
                .setContentText(p.collapsed())
                .setStyle(new Notification.BigTextStyle().bigText(p.expanded()))
                .setSmallIcon(icon())
                .setColor(0xFF1E88E5)
                .setOngoing(true)
                .setOnlyAlertOnce(true)
                .setShowWhen(false)
                .setLocalOnly(true)
                .setCategory(Notification.CATEGORY_SERVICE)
                .setVisibility(Notification.VISIBILITY_SECRET);
        PendingIntent pi = tap();
        if (pi != null) b.setContentIntent(pi);
        Notification n = b.build();
        n.flags |= Notification.FLAG_NO_CLEAR | Notification.FLAG_ONGOING_EVENT;
        // 通知栏显示 "XRouter" 而不是 "Android 系统"（与 lspd 的 substName 相同）
        n.extras.putString("android.substName", "XRouter");
        return n;
    }

    void post(Traffic.Payload p) throws Exception {
        Notification n = build(p);
        try {
            ensureChannel();
            call(nm(), "enqueueNotificationWithTag", PKG, PKG, TAG, ID, n, 0);
        } catch (Exception e) {
            // binder 断开（system_server 重启）时重取一次
            nm = null;
            ensureChannel();
            call(nm(), "enqueueNotificationWithTag", PKG, PKG, TAG, ID, n, 0);
        }
    }

    void cancel() throws Exception {
        Object m = nm();
        try {
            call(m, "cancelNotificationWithTag", PKG, PKG, TAG, ID, 0);
        } catch (NoSuchMethodException e) {
            call(m, "cancelNotificationWithTag", PKG, TAG, ID, 0);
        }
    }
}
