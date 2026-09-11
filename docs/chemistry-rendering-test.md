# 化学科研渲染回归文档

这些是小体积、可重复的验证材料；不是一次打开 100 MB 或启动 80 个 AI 的压力脚本。测试直接读取本文件中的数学和 SMILES 块。先在单窗口验证，再采用受限的模型测试验证多会话和总预算。不要为测试自动请求真实 AI 服务。

成功的 SMILES 必须是单行、单个分子，源码不超过 2048 字节，最多 128 个原子和 192 条键。反应 SMILES、立体化学、坏结构和专用格式都单独标记源码回退，不能把回退文本当成结构图通过。

每次检查：往返滚动、反复拖选文字、连续改变窗口宽高、切换标签后返回、切换主题；记录有无丢图、闪烁、错误重绘、选区错位和内存持续增长。看到不支持的语法时应保留源码，不能悄悄生成错误科学图形。

## 基础化学方程与二维结构

普通 TeX 描述上下标与箭头；SMILES 给出明确的二维连接关系。

$$
2\mathrm{H}_2+\mathrm{O}_2\rightarrow 2\mathrm{H}_2\mathrm{O}
$$

### 乙醇

```smiles
CCO
```

### 苯

```smiles
c1ccccc1
```

### 阿司匹林

```smiles
CC(=O)Oc1ccccc1C(=O)O
```

### 咖啡因

```smiles
Cn1c(=O)c2c(ncn2C)n(C)c1=O
```

### 甘氨酸

```smiles
NCC(=O)O
```

### 乙酸

```smiles
CC(=O)O
```

### 氯化铵

```smiles
[NH4+].[Cl-]
```

### 萘

```smiles
c1ccc2ccccc2c1
```

## 常用化学公式

以下是应进入普通数学渲染的基础化学式；下标、上标和电荷都保留在 TeX 源码中。方括号表示浓度或离子，不是 SMILES 输入。

内联浓度关系为 $c=\frac{n}{V}$，酸碱平衡常数为 $K_a=\frac{[\mathrm{H}^{+}][\mathrm{A}^{-}]}{[\mathrm{HA}]}$。

$$
2\mathrm{H}_2+\mathrm{O}_2\rightarrow2\mathrm{H}_2\mathrm{O}
$$

$$
\mathrm{NH}_4^{+}\leftrightarrow\mathrm{NH}_3+\mathrm{H}^{+},\qquad
K_b=\frac{[\mathrm{NH}_4^{+}]}{[\mathrm{NH}_3][\mathrm{H}^{+}]}
$$

$$
\mathrm{Fe}^{2+}+2\mathrm{OH}^{-}\rightarrow\mathrm{Fe(OH)}_2,\qquad
{}^{13}\mathrm{C}-\mathrm{NMR}\;\delta=31\,\mathrm{ppm}
$$

$$
\Delta G^{\circ}_{\mathrm{rxn}}=\Delta H^{\circ}_{\mathrm{rxn}}-T\Delta S^{\circ}_{\mathrm{rxn}},\qquad
\Delta G=\Delta G^{\circ}+RT\ln Q
$$

## SMILES 覆盖

这些单行块覆盖芳香环、稠环、支链、离子、断连盐和同位素。它们是合成的输入样例，不表达实验结果。

### 芳香杂环

```smiles
c1ccncc1
```

### 支链与羧酸

```smiles
CC(C)C(=O)O
```

### 离子与断连盐

```smiles
C[N+](C)(C)C.[Cl-]
```

### 同位素甲烷

```smiles
[13CH4]
```

### 氘水

```smiles
[2H]O[2H]
```

`[13CH4]` 和 `[2H]O[2H]` 是有效的同位素 SMILES，需由生产解析器实际验证；若当前依赖不接受同位素属性，测试应记录源码回退原因，不得改写成普通氢原子。

## 超过单面板缓存项数的不同结构

直链烷烃是布局负载样例，不用于比较化学性质。每段混合重复公式、不同分子和相同本地图片，验证资源复用。

### 链长样例 01

第 1 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
C
```

$$
K_{1}=\frac{[P]}{[R]}\qquad \Delta G_{1}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 02

第 2 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CC
```

$$
K_{2}=\frac{[P]}{[R]}\qquad \Delta G_{2}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 03

第 3 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCC
```

$$
K_{3}=\frac{[P]}{[R]}\qquad \Delta G_{3}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 04

第 4 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCC
```

$$
K_{4}=\frac{[P]}{[R]}\qquad \Delta G_{4}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 05

第 5 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCC
```

$$
K_{5}=\frac{[P]}{[R]}\qquad \Delta G_{5}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 06

第 6 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCC
```

$$
K_{6}=\frac{[P]}{[R]}\qquad \Delta G_{6}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 07

第 7 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCC
```

$$
K_{7}=\frac{[P]}{[R]}\qquad \Delta G_{7}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 08

第 8 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCC
```

$$
K_{8}=\frac{[P]}{[R]}\qquad \Delta G_{8}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 09

第 9 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCC
```

$$
K_{9}=\frac{[P]}{[R]}\qquad \Delta G_{9}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 10

第 10 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCC
```

$$
K_{10}=\frac{[P]}{[R]}\qquad \Delta G_{10}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 11

第 11 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCC
```

$$
K_{11}=\frac{[P]}{[R]}\qquad \Delta G_{11}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 12

第 12 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCC
```

$$
K_{12}=\frac{[P]}{[R]}\qquad \Delta G_{12}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 13

第 13 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCC
```

$$
K_{13}=\frac{[P]}{[R]}\qquad \Delta G_{13}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 14

第 14 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCC
```

$$
K_{14}=\frac{[P]}{[R]}\qquad \Delta G_{14}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 15

第 15 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCC
```

$$
K_{15}=\frac{[P]}{[R]}\qquad \Delta G_{15}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 16

第 16 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCC
```

$$
K_{16}=\frac{[P]}{[R]}\qquad \Delta G_{16}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 17

第 17 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCC
```

$$
K_{17}=\frac{[P]}{[R]}\qquad \Delta G_{17}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 18

第 18 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCC
```

$$
K_{18}=\frac{[P]}{[R]}\qquad \Delta G_{18}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 19

第 19 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCC
```

$$
K_{19}=\frac{[P]}{[R]}\qquad \Delta G_{19}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 20

第 20 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCC
```

$$
K_{20}=\frac{[P]}{[R]}\qquad \Delta G_{20}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 21

第 21 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCC
```

$$
K_{21}=\frac{[P]}{[R]}\qquad \Delta G_{21}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 22

第 22 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{22}=\frac{[P]}{[R]}\qquad \Delta G_{22}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 23

第 23 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{23}=\frac{[P]}{[R]}\qquad \Delta G_{23}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 24

第 24 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{24}=\frac{[P]}{[R]}\qquad \Delta G_{24}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 25

第 25 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{25}=\frac{[P]}{[R]}\qquad \Delta G_{25}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 26

第 26 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{26}=\frac{[P]}{[R]}\qquad \Delta G_{26}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 27

第 27 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{27}=\frac{[P]}{[R]}\qquad \Delta G_{27}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 28

第 28 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{28}=\frac{[P]}{[R]}\qquad \Delta G_{28}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 29

第 29 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{29}=\frac{[P]}{[R]}\qquad \Delta G_{29}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 30

第 30 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{30}=\frac{[P]}{[R]}\qquad \Delta G_{30}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 31

第 31 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{31}=\frac{[P]}{[R]}\qquad \Delta G_{31}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 32

第 32 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{32}=\frac{[P]}{[R]}\qquad \Delta G_{32}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 33

第 33 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{33}=\frac{[P]}{[R]}\qquad \Delta G_{33}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 34

第 34 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{34}=\frac{[P]}{[R]}\qquad \Delta G_{34}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 35

第 35 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{35}=\frac{[P]}{[R]}\qquad \Delta G_{35}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 36

第 36 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{36}=\frac{[P]}{[R]}\qquad \Delta G_{36}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 37

第 37 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{37}=\frac{[P]}{[R]}\qquad \Delta G_{37}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 38

第 38 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{38}=\frac{[P]}{[R]}\qquad \Delta G_{38}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 39

第 39 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{39}=\frac{[P]}{[R]}\qquad \Delta G_{39}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 40

第 40 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{40}=\frac{[P]}{[R]}\qquad \Delta G_{40}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 41

第 41 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{41}=\frac{[P]}{[R]}\qquad \Delta G_{41}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 42

第 42 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{42}=\frac{[P]}{[R]}\qquad \Delta G_{42}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 43

第 43 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{43}=\frac{[P]}{[R]}\qquad \Delta G_{43}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 44

第 44 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{44}=\frac{[P]}{[R]}\qquad \Delta G_{44}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 45

第 45 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{45}=\frac{[P]}{[R]}\qquad \Delta G_{45}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 46

第 46 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{46}=\frac{[P]}{[R]}\qquad \Delta G_{46}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 47

第 47 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{47}=\frac{[P]}{[R]}\qquad \Delta G_{47}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

### 链长样例 48

第 48 次结构预览。上下往返滚动并拖选这段文字，检查图片出现后段落锚点是否移动。缩窄窗口后恢复，分子不能污染其他块或下一个标签。

```smiles
CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC
```

$$
K_{48}=\frac{[P]}{[R]}\qquad \Delta G_{48}=\Delta G^\circ+RT\ln Q
$$

![复用的本地小图片](screenshots/scientific-fixture.png)

## 不支持输入必须保留源码

### 手性标记

<!-- pebrel-test: source-fallback -->
```smiles
C[C@H](O)C(=O)O
```

### 双键立体方向

<!-- pebrel-test: source-fallback -->
```smiles
F/C=C/F
```

### 非闭合环

<!-- pebrel-test: source-fallback -->
```smiles
C1CC
```

### mhchem 当前尚未实现

<!-- pebrel-test: source-fallback -->
$$
\ce{2H2 + O2 -> 2H2O}
$$

### 分子名不应被当成 SMILES

普通段落提到 smiles 或 caffeine，不应自行触发分子图。

## 反应式、立体化学与坏结构

生产渲染器只接收一个分子。反应 SMILES 仍是可复制的科学源码；它不能被误画成单个分子。带 `@`、`/` 或 `\` 的手性输入，以及解析失败的结构，都必须保留原文。

### 反应 SMILES（源码回退）

<!-- pebrel-test: source-fallback -->
```smiles
CC(=O)O.OCC>>CC(=O)OCC.O
```

### 反应 SMILES（含试剂段，源码回退）

<!-- pebrel-test: source-fallback -->
```smiles
CC(=O)O.OCC>O>CC(=O)OCC
```

### 手性中心（`@`，源码回退）

<!-- pebrel-test: source-fallback -->
```smiles
C[C@H](O)C(=O)O
```

### 烯烃方向（`/`，源码回退）

<!-- pebrel-test: source-fallback -->
```smiles
F/C=C/F
```

### 烯烃方向（反斜杠，源码回退）

<!-- pebrel-test: source-fallback -->
```smiles
F/C=C\F
```

### 不闭合环（坏结构）

<!-- pebrel-test: source-fallback -->
```smiles
C1CC
```

### 未闭合分支（坏结构）

<!-- pebrel-test: source-fallback -->
```smiles
C(C
```

## 专用结构格式仅作源码对照

MOL/SDF 需要专用解析和布局，目前只放普通代码块来检查复制、滚动和选区稳定性；遇到这些格式时应回退到原始源码，这不是已支持的分子渲染格式。

```mol
synthetic_mol_fixture
  Nebula  090826  2D

  2  1  0  0  0  0  0  0  0  0  0  0  0  0  0  0
    0.0000    0.0000    0.0000 C   0  0  0  0  0  0  0  0  0  0  0  0
    1.5400    0.0000    0.0000 O   0  0  0  0  0  0  0  0  0  0  0  0
  1  2  1  0  0  0  0
M  END
```

```sdf
synthetic_sdf_fixture
  Nebula  090826

  2  1  0  0  0  0  0  0  0  0  0  0  0  0  0  0
    0.0000    0.0000    0.0000 C   0  0  0  0  0  0  0  0  0  0  0  0
    1.5400    0.0000    0.0000 O   0  0  0  0  0  0  0  0  0  0  0  0
  1  2  1  0  0  0  0
M  END
$$$$
```

## 尚未实现的化学 TeX 命令

以下命令属于化学单位/化学式扩展，不是普通数学。每个块都必须留在源码中。

### mhchem 化学式

<!-- pebrel-test: source-fallback -->
$$
\ce{2H2 + O2 -> 2H2O}
$$

### siunitx 单位

<!-- pebrel-test: source-fallback -->
$$
\SI{25}{\degreeCelsius}
$$

### siunitx 纯单位

<!-- pebrel-test: source-fallback -->
$$
\pu{1.00e3\,kJ\,mol^{-1}}
$$
