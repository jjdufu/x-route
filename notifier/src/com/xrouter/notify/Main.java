package com.xrouter.notify;

import android.content.Context;
import android.os.Looper;

import java.io.File;
import java.io.FileOutputStream;
import java.io.FileInputStream;
import java.io.ByteArrayOutputStream;
import java.nio.charset.StandardCharsets;

/**
 * XRouter 常驻通知进程（无 APK）。由守护进程以 root 经 app_process 拉起：
 *   app_process -Djava.class.path=xrnotify.jar /system/bin --nice-name=xrnotify com.xrouter.notify.Main serve <run>
 *   ... com.xrouter.notify.Main cancel        一次性撤掉通知
 *   ... com.xrouter.notify.Main zip <out.zip> <目录> <子目录名>   把日志目录打包成 zip（安卓没有 zip 命令，见 xr make_zip）
 * serve：每秒读一次 <run>/notify.req（守护进程原子写入的一行负载，或 "cancel"），
 * 内容变化才发通知；处理结果写 <run>/notify.ack（"ok <负载>" 或 "err <原因>"）。
 * 收到 cancel、<run> 被删除（卸载）时撤通知并退出。异常直接退出，由守护进程重新拉起。
 */
public final class Main {
    private Main() {}

    static void log(String s) {
        System.err.println(s);
    }

    private static Context systemContext() throws Exception {
        Class<?> at = Class.forName("android.app.ActivityThread");
        Object t = at.getMethod("systemMain").invoke(null);
        return (Context) at.getMethod("getSystemContext").invoke(t);
    }

    static String read(File f) {
        try (FileInputStream in = new FileInputStream(f)) {
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            byte[] buf = new byte[256];
            int n;
            while ((n = in.read(buf)) > 0 && out.size() < 4096) out.write(buf, 0, n);
            return new String(out.toByteArray(), StandardCharsets.UTF_8).trim();
        } catch (Exception e) {
            return null;
        }
    }

    static void write(File f, String s) {
        File tmp = new File(f.getPath() + ".tmp");
        try (FileOutputStream out = new FileOutputStream(tmp)) {
            out.write((s + "\n").getBytes(StandardCharsets.UTF_8));
        } catch (Exception e) {
            return;
        }
        if (!tmp.renameTo(f)) tmp.delete();
    }

    @SuppressWarnings("deprecation")
    public static void main(String[] args) {
        Thread.setDefaultUncaughtExceptionHandler((th, e) -> {
            log("未捕获异常：" + e);
            System.exit(1);
        });
        String cmd = args.length > 0 ? args[0] : "";
        if ("zip".equals(cmd)) {
            // 只用 java.util.zip，不碰系统服务
            if (args.length < 4) System.exit(2);
            try {
                Zipper.zipDir(new File(args[1]), new File(args[2]), args[3]);
                System.exit(0);
            } catch (Throwable t) {
                log("打包失败：" + t);
                new File(args[1]).delete();
                System.exit(1);
            }
            return;
        }
        if (!"serve".equals(cmd) && !"cancel".equals(cmd)) {
            log("用法：Main serve <run 目录> | cancel | zip <out> <目录> <子目录名>");
            System.exit(2);
        }
        Notifier n;
        try {
            if (Looper.getMainLooper() == null) Looper.prepareMainLooper();
            n = new Notifier(systemContext());
        } catch (Throwable t) {
            log("初始化失败：" + t);
            System.exit(1);
            return;
        }
        if ("cancel".equals(cmd)) {
            try {
                n.cancel();
                System.exit(0);
            } catch (Throwable t) {
                log("撤通知失败：" + t);
                System.exit(1);
            }
            return;
        }
        if (args.length < 2) System.exit(2);
        serve(n, new File(args[1]));
    }

    private static void serve(Notifier n, File run) {
        File req = new File(run, "notify.req");
        File ack = new File(run, "notify.ack");
        String last = null;
        log("已启动");
        while (true) {
            if (!run.isDirectory()) {
                // 数据目录被删除（卸载）：撤通知后退出
                try { n.cancel(); } catch (Throwable ignored) { }
                System.exit(0);
            }
            String line = read(req);
            if (line != null && !line.isEmpty() && !line.equals(last)) {
                last = line;
                if ("cancel".equals(line)) {
                    try {
                        n.cancel();
                        write(ack, "ok cancel");
                    } catch (Throwable t) {
                        write(ack, "err " + t);
                    }
                    System.exit(0);
                }
                Traffic.Payload p = Traffic.parse(line);
                if (p == null) {
                    write(ack, "err 负载格式不对：" + line);
                } else {
                    try {
                        n.post(p);
                        write(ack, "ok " + line);
                    } catch (Throwable t) {
                        Throwable c = t.getCause() != null ? t.getCause() : t;
                        log("发通知失败：" + c);
                        write(ack, "err " + c);
                        System.exit(1);
                    }
                }
            }
            try {
                Thread.sleep(1000);
            } catch (InterruptedException e) {
                return;
            }
        }
    }
}
