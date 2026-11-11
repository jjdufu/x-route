package com.xrouter.notify;

import java.util.Locale;

/**
 * 累计流量文案与守护进程负载解析。纯 Java，不依赖 Android，可在普通 JVM 上单测。
 * 数值格式与 traffic.sh 的 acct_human 一致：1024 进制、一位小数。
 */
final class Traffic {
    private static final String[] UNITS = {"B", "KB", "MB", "GB", "TB"};

    /** 守护进程写入 notify.req 的一行：状态 显示项 公网上传 公网下载 内网上传 内网下载。 */
    static final class Payload {
        final String state;
        final String mode;
        final long wanUp, wanDn, lanUp, lanDn;

        Payload(String state, String mode, long wanUp, long wanDn, long lanUp, long lanDn) {
            this.state = state;
            this.mode = mode;
            this.wanUp = wanUp;
            this.wanDn = wanDn;
            this.lanUp = lanUp;
            this.lanDn = lanDn;
        }

        String title() {
            return "XRouter · " + state;
        }

        String collapsed() {
            return Traffic.collapsed(mode, wanUp, wanDn, lanUp, lanDn);
        }

        String expanded() {
            return Traffic.expanded(wanUp, wanDn, lanUp, lanDn);
        }
    }

    private Traffic() {}

    static String human(long bytes) {
        if (bytes < 0) bytes = 0;
        double v = bytes;
        int i = 0;
        while (v >= 1024 && i < UNITS.length - 1) {
            v /= 1024;
            i++;
        }
        if (i == 0) return bytes + " B";
        return String.format(Locale.ROOT, "%.1f %s", v, UNITS[i]);
    }

    static String pair(long up, long down) {
        return "↑" + human(up) + " ↓" + human(down);
    }

    /** 收起时的一行：mode = total | wan | lan。 */
    static String collapsed(String mode, long wanUp, long wanDn, long lanUp, long lanDn) {
        if ("wan".equals(mode)) return "公网 " + pair(wanUp, wanDn);
        if ("lan".equals(mode)) return "内网 " + pair(lanUp, lanDn);
        return pair(wanUp + lanUp, wanDn + lanDn);
    }

    /** 展开后的两行：公网、内网。 */
    static String expanded(long wanUp, long wanDn, long lanUp, long lanDn) {
        return "公网 " + pair(wanUp, wanDn) + "\n内网 " + pair(lanUp, lanDn);
    }

    private static long num(String s) {
        try {
            long v = Long.parseLong(s);
            return v < 0 ? 0 : v;
        } catch (NumberFormatException e) {
            return 0;
        }
    }

    /** 解析一行负载；为空或字段不足返回 null。未知显示项按 total 处理，坏数字按 0。 */
    static Payload parse(String line) {
        if (line == null) return null;
        String[] f = line.trim().split("\\s+");
        if (f.length < 6 || f[0].isEmpty()) return null;
        String mode = f[1];
        if (!"total".equals(mode) && !"wan".equals(mode) && !"lan".equals(mode)) mode = "total";
        return new Payload(f[0], mode, num(f[2]), num(f[3]), num(f[4]), num(f[5]));
    }
}
