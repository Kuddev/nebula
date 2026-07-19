# Native Math Rendering Test

这是一份给 Nebula Markdown 阅读器使用的数学渲染截图样例。本文只把成对的 `$...$` 和 `$$...$$` 识别为数学公式；裸露 TeX 命令仍然按普通 Markdown 文本显示。

## 1. 行内公式

行内公式应该和中文、英文正常混排：圆的面积是 $S=\pi r^2$，勾股定理是 $a^2+b^2=c^2$，欧拉恒等式是 $e^{i\pi}+1=0$。这里再放一个带上下标的表达式 $x_{i+1}^2+y_{j-1}^2$，检查基线、间距和换行。

## 2. 希腊字母与常用运算符

$$
\alpha+\beta+\gamma+\Delta+\Theta+\Lambda+\Omega
$$

$$
\pm\quad \mp\quad \times\quad \div\quad \cdot\quad \ast\quad \circ\quad \bullet\quad \oplus\quad \otimes
$$

## 3. 关系符号与集合

$$
x\ne y,\quad x\neq y,\quad x\approx y,\quad x\equiv y,\quad x\leq y,\quad x\geq y
$$

$$
A\subseteq B,\quad B\supseteq A,\quad A\cap B,\quad A\cup B,\quad x\in A,\quad x\notin B,\quad \varnothing
$$

## 4. 箭头

$$
a\to b\quad b\leftarrow a\quad A\leftrightarrow B\quad P\Rightarrow Q\quad P\Leftrightarrow Q
$$

下面这行中的 `->` 是普通 Markdown 文本，不应该被识别成数学：`source -> target`。

## 5. 分式、根式与二项式

$$
\frac{1}{2}+\frac{x+1}{x-1}=\frac{x^2+1}{x^2-1}
$$

$$
\sqrt{x^2+y^2}\quad \sqrt[3]{x^3+y^3}\quad \sqrt{\frac{a+b}{c+d}}\quad \binom{n}{k}
$$

## 6. 上下标与重音

$$
\sum_{k=1}^{n}k^2,\quad \prod_{i=1}^{n}x_i,\quad a_0+a_1x+a_2x^2+\cdots+a_nx^n
$$

$$
\hat{x}
$$

$$
\vec{v}
$$

$$
\dot{x}\quad \ddot{x}
$$

## 7. 极限、导数与积分

$$
\lim_{x\to 0}\frac{\sin x}{x}=1
$$

$$
\frac{\mathrm{d}}{\mathrm{d}x}\left(x^3+2x\right)=3x^2+2,\quad
\frac{\partial^2 f}{\partial x\partial y}
$$

$$
\int_{0}^{1}x^2\,\mathrm{d}x=\frac{1}{3},\quad
\iint_{D}(x+y)\,\mathrm{d}x\,\mathrm{d}y,\quad
\oint_C\vec{F}\cdot\mathrm{d}\vec{r}
$$

## 8. 伸缩括号与绝对值

$$
\left(\frac{a}{b}\right),\quad
\left[\sum_{i=1}^{n}x_i\right],\quad
\left\{x\in\mathbb{R}\mid x>0\right\},\quad
\left\lvert x-1\right\rvert<\varepsilon
$$

## 9. 矩阵

$$
\begin{matrix}
a & b \\
c & d
\end{matrix}
\qquad
\begin{pmatrix}
1 & 0 & 0 \\
0 & 1 & 0 \\
0 & 0 & 1
\end{pmatrix}
$$

$$
\begin{bmatrix}
1 & 2 & 3 \\
4 & 5 & 6
\end{bmatrix}
\qquad
\begin{vmatrix}
a & b \\
c & d
\end{vmatrix}=ad-bc
$$

## 10. 分段函数与多行公式

$$
f(x)=\begin{cases}
x^2, & x\ge 0 \\
-x^2, & x<0
\end{cases}
$$

$$
\begin{aligned}
(a+b)^2 &= a^2+2ab+b^2 \\
(a-b)^2 &= a^2-2ab+b^2
\end{aligned}
$$

## 11. 逻辑、偏导与向量分析

$$
\forall\varepsilon>0,\quad \exists\delta>0,\quad
0<\lvert x-a\rvert<\delta\Rightarrow\lvert f(x)-f(a)\rvert<\varepsilon
$$

$$
\nabla\cdot\vec{E}=\frac{\rho}{\varepsilon_0},\quad
\nabla\times\vec{B}=\mu_0\vec{J}+\mu_0\varepsilon_0\frac{\partial\vec{E}}{\partial t}
$$

## 12. 长公式换行与收缩

下面的块级公式用于检查过宽公式是否会自动收缩到阅读列内，而不是越过右边界：

$$
\frac{\displaystyle\sum_{i=1}^{n}\left(\alpha_i x_i+\beta_i y_i\right)}{\sqrt{\displaystyle\prod_{j=1}^{m}\left(1+z_j^2\right)}}
\leq
\left\lvert\int_{0}^{1}\frac{e^{t^2}}{1+t^2}\,\mathrm{d}t\right\rvert+\left\lvert\oint_C\vec{F}\cdot\mathrm{d}\vec{r}\right\rvert
$$

## 13. 空行与 Unicode 说明文字

下面的公式中故意保留空行和中文说明，检查块级 `$$` 的跨行处理：

$$
\sin(a+b)=\sin a\cos b+\cos a\sin b

这里是公式内部的中文说明：和角公式。

\cos(a+b)=\cos a\cos b-\sin a\sin b
$$

## 14. 边界对照

下面这些内容不应该生成数学字形，因为没有使用 `$` 或 `$$` 围栏：

```text
\lim_{x \to 0} \frac{\sin x}{x}
\sqrt{x^2+y^2}
\frac{1}{2}
```

最后的明确行内公式应该生成数学字形：$\lim_{x\to 0}\frac{\sin x}{x}=1$。
