<?php
$s=1024;$im=imagecreatetruecolor($s,$s);imagesavealpha($im,true);
imagealphablending($im,false);
imagefill($im,0,0,imagecolorallocatealpha($im,0,0,0,127));
imagealphablending($im,true);
$bg=imagecolorallocate($im,255,90,60);
imagefilledrectangle($im,100,200,$s-100,$s-200,$bg);
imagefilledrectangle($im,200,100,$s-200,$s-100,$bg);
foreach([[200,300],[$s-200,300],[200,$s-300],[$s-200,$s-300]] as [$x,$y]) imagefilledellipse($im,$x,$y,200,200,$bg);
$w=imagecolorallocate($im,255,255,255);
imagesetthickness($im,60);
imagearc($im,$s/2,$s/2,420,420,0,360,$w);
imagefilledellipse($im,$s/2,$s/2,140,140,$w);
imagepng($im,__DIR__.'/icon.png');
