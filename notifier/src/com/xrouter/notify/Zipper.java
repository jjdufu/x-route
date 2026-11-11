package com.xrouter.notify;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.util.Arrays;
import java.util.zip.ZipEntry;
import java.util.zip.ZipOutputStream;

/** 日志导出打包为 zip（Deflate）：条目按名称排序，路径以 <子目录名>/ 开头，与 zip -r 的结构一致。 */
final class Zipper {
    private Zipper() {}

    static void zipDir(File out, File base, String name) throws IOException {
        File root = new File(base, name);
        if (!root.isDirectory()) throw new IOException("不是目录：" + root);
        File tmp = new File(out.getPath() + ".part");
        try (ZipOutputStream zos = new ZipOutputStream(new FileOutputStream(tmp))) {
            zos.setLevel(9);
            add(zos, root, name + "/");
        }
        if (!tmp.renameTo(out)) {
            tmp.delete();
            throw new IOException("无法写入 " + out);
        }
    }

    private static void add(ZipOutputStream zos, File dir, String prefix) throws IOException {
        zos.putNextEntry(new ZipEntry(prefix));
        zos.closeEntry();
        File[] list = dir.listFiles();
        if (list == null) return;
        Arrays.sort(list);
        byte[] buf = new byte[16384];
        for (File f : list) {
            if (f.isDirectory()) {
                add(zos, f, prefix + f.getName() + "/");
                continue;
            }
            ZipEntry e = new ZipEntry(prefix + f.getName());
            e.setTime(f.lastModified());
            zos.putNextEntry(e);
            try (FileInputStream in = new FileInputStream(f)) {
                int n;
                while ((n = in.read(buf)) > 0) zos.write(buf, 0, n);
            }
            zos.closeEntry();
        }
    }
}
