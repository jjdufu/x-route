package com.xrouter.keepalive;

import java.util.Locale;

/** 累计流量的显示文案（与 traffic.sh 的 acct_human 一致：1024 进制、一位小数）。 */
final class Traffic {
    private static final String[] UNITS = {"B", "KB", "MB", "GB", "TB"};

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
}
