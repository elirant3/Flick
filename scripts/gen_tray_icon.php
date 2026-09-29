<?php
// Menu bar icon for Flick: black shapes on transparency, used as a macOS "template" image so
// the system tints it for light and dark menu bars. Crop marks around a shutter dot, matching
// the app icon. Drawn at 8x and downsampled for clean anti-aliasing.
// Output: src-tauri/icons/tray-template.png (44x44 = 22pt @2x). Run: php scripts/gen_tray_icon.php

$out = 44;
$k = 8;
$s = $out * $k;
$im = imagecreatetruecolor($s, $s);
imagesavealpha($im, true);
imagealphablending($im, false);
imagefill($im, 0, 0, imagecolorallocatealpha($im, 0, 0, 0, 127));
imagealphablending($im, true);
$ink = imagecolorallocate($im, 0, 0, 0);

// Four corner brackets.
$m = 5 * $k;      // margin
$len = 11 * $k;   // arm length
$t = 3 * $k;      // stroke
$e = $s - $m;  // outer edge on the far side
// Each corner: [outer x, outer y, x direction inward, y direction inward].
foreach ([[$m, $m, 1, 1], [$e, $m, -1, 1], [$m, $e, 1, -1], [$e, $e, -1, -1]] as [$x, $y, $dx, $dy]) {
    $box = function (int $x1, int $y1, int $x2, int $y2) use ($im, $ink) {
        imagefilledrectangle($im, min($x1, $x2), min($y1, $y2), max($x1, $x2) - 1, max($y1, $y2) - 1, $ink);
    };
    $box($x, $y, $x + $dx * $len, $y + $dy * $t);  // horizontal arm
    $box($x, $y, $x + $dx * $t, $y + $dy * $len);  // vertical arm
}

// Shutter: a ring with a dot.
$c = intdiv($s, 2);
imagesetthickness($im, 1);
imagefilledellipse($im, $c, $c, 20 * $k, 20 * $k, $ink);
imagealphablending($im, false);
imagefilledellipse($im, $c, $c, 14 * $k, 14 * $k, imagecolorallocatealpha($im, 0, 0, 0, 127));
imagealphablending($im, true);
imagefilledellipse($im, $c, $c, 7 * $k, 7 * $k, $ink);

$small = imagecreatetruecolor($out, $out);
imagesavealpha($small, true);
imagealphablending($small, false);
imagefill($small, 0, 0, imagecolorallocatealpha($small, 0, 0, 0, 127));
imagecopyresampled($small, $im, 0, 0, 0, 0, $out, $out, $s, $s);
imagepng($small, __DIR__ . '/../src-tauri/icons/tray-template.png');
echo "wrote src-tauri/icons/tray-template.png\n";
