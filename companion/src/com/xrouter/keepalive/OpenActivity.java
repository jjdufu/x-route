package com.xrouter.keepalive;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;

import java.io.File;
import java.io.FileOutputStream;

/**
 * 通知点击跳板（无界面）。管理器的 WebUIActivity 未导出，普通应用无法直接打开，
 * 因此只在自己的 files 目录写 open.req，由 root 守护进程（每秒检查）打开模块 WebUI。
 * 若上一次的请求超过 3 秒仍未被取走（守护进程不在），改为直接打开管理器首页。
 */
public class OpenActivity extends Activity {
    private static final String[] MANAGERS = {"com.sukisu.ultra", "me.weishu.kernelsu", "com.rifsxd.ksunext"};

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        File req = new File(getFilesDir(), "open.req");
        boolean stale = req.exists() && System.currentTimeMillis() - req.lastModified() > 3000;
        if (stale) {
            req.delete();
            openManager();
        } else {
            try (FileOutputStream out = new FileOutputStream(req)) {
                out.write('1');
            } catch (Exception ignored) {
                openManager();
            }
        }
        finish();
    }

    private void openManager() {
        for (String pkg : MANAGERS) {
            Intent i = getPackageManager().getLaunchIntentForPackage(pkg);
            if (i == null) continue;
            i.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            try {
                startActivity(i);
                return;
            } catch (Exception ignored) {
                // 试下一个
            }
        }
    }
}
