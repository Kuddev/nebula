# 跨尺度代谢动力学、分子表征与注意力预测：科研渲染验收语料

**文档类型：** 方法学论文式渲染验收示例　　**版本：** 0.1（合成数据） \
**用途：** Pebrel/Nebula Markdown 与 AI CLI 的科学内容渲染验收输入 \
**声明：** 本文是可重复的排版、解析、滚动和回退测试夹具，不是研究论文，也不报告真实科研发现。本文中的观测值、分子式、损失、置信区间和预测性能均由本文件为测试而构造；“准确”“稳定”“通过”等词只描述数学例子的内部一致性，不描述产品性能已经达标。

## 摘要

科学工作流常把不同尺度的对象放在同一个阅读表面：分子结构以 SMILES 表示，代谢通量以微分方程表示，时间序列以傅里叶基表示，而模型则以 Transformer 的注意力权重汇总上下文。它们有不同的语法边界，却会在同一份 Markdown 文档中相邻出现。本示例构造一套小型、完全合成的“代谢动力学—分子表征—注意力预测”实验，用来检验渲染器能否同时处理中文、English mixed text、窄窗口中的长公式、多行对齐推导、矩阵、分段函数、概率统计、代码、表格、Markdown 图片和 SMILES 化学结构。

方法的核心是把一个可解释的动力学状态 $x(t)$ 转换为离散观测 $z_k$，以分子描述符 $m$ 和时间位置编码 $p_k$ 组成 token，再用注意力读出合成的未来通量。我们给出从守恒方程、指数衰减、傅里叶变换到 scaled dot-product attention 的连续推导，并用多组扰动参数检查长文滚动时的结构重复与缓存变化。结果章节只列出生成器预先指定的值，例如合成样本的均方误差和 bootstrap 区间；这些值不构成模型在现实数据上的性能证据。文末的验收附录列出滚动往返、跨文字选区、resize、切 tab 返回、缺图和无效语法回退等观察点，并明确 80 个真实 AI 会话与 100 MB 大负载需要独立预算，本文不能替代那类压力测试。

**关键词：** scientific rendering；synthetic fixture；跨尺度动力学；傅里叶变换；Transformer attention；SMILES；可重复验收

## 1. 引言

### 1.1 问题背景

在科研记录中，阅读者经常在相邻的段落之间切换抽象层级。一个小分子可能先以名称出现，再以键图或 SMILES 出现；同一系统的浓度变化可能先写成表格，再写成带积分的守恒关系；模型解释可能同时需要注意力矩阵、概率分布和误差统计。如果终端或桌面 Markdown 视图只正确绘制其中一种内容，用户会在复制、滚动或切换标签页时丢失上下文。

本文件因此选择一个足够小但跨语法的合成问题。我们设代谢池 $A$、中间体 $B$ 和产物 $P$ 的量分别为 $a(t)$、$b(t)$、$p(t)$，并令输入通量为 $u(t)$。最小反应链可以写成

$$
A \xrightarrow{k_1} B \xrightarrow{k_2} P,
\qquad
\frac{d}{dt}\begin{bmatrix}a(t)\\b(t)\\p(t)\end{bmatrix}
=
\begin{bmatrix}-k_1 & 0 & 0\\ k_1 & -k_2 & 0\\ 0 & k_2 & 0\end{bmatrix}
\begin{bmatrix}a(t)\\b(t)\\p(t)\end{bmatrix}
+\begin{bmatrix}u(t)\\0\\0\end{bmatrix}.
\tag{1}
$$

式（1）同时覆盖了长矩阵、下标、导数、箭头和真正的行分隔符 `\\`。符号 $k_1,k_2>0$ 只是合成速率常数；它们没有从实验室测量中估计出来。

### 1.2 验收问题

本研究式夹具回答的是渲染问题，而非生物学问题：

1. 行内和块级数学是否保持定界符、等号、上下标、希腊字母、积分和求和？
2. 多行 `aligned`、`cases`、矩阵和窄窗口长公式是否保持行距，不覆盖相邻文字？
3. 化学解析器能否把芳香环、分支、离子和同位素源码展示为结构；遇到手性或未知命令时，是否清楚回退到源码？
4. Markdown 图片、表格、代码和双语文字能否与公式在同一滚动上下文中共存？
5. 缓存、窗口尺寸和 tab 生命周期变化时，内容是否仍能重新排版，而不是依赖本次会话的偶然状态？

### 1.3 边界声明

“预测”一词在本文中只指一个由确定性脚本生成的数值序列。合成序列没有患者、样本、实验仪器或外部数据库，也没有通过网络服务生成。文中所有数字都可以由方法章节中的参数重新算出。尤其是表 4 中的 $R^2$、RMSE 和区间，不应被解释为真实模型的泛化能力。

## 2. 方法

### 2.1 符号与尺度

时间变量 $t$ 的单位是 arbitrary time unit，浓度变量以 normalized concentration 表示。为避免把单位排版当作科学结论，我们只使用无量纲组合 $\theta=k_2t$ 和 $\rho=k_1/k_2$。指数衰减的基本解为

$$
\theta = k_2 t,
\qquad
a(t)=a_0e^{-k_1t},
\qquad
\rho=\frac{k_1}{k_2},
\qquad
\lim_{t\to\infty}a(t)=0.
\tag{2}
$$

当 $k_1=k_2=k$ 时，连续链的中间体项出现线性因子。为使边界条件可见，我们从积分因子开始：

$$
\begin{aligned}
\frac{db}{dt}+kb &= ka_0e^{-kt},\\
\frac{d}{dt}\left(e^{kt}b(t)\right)&=ka_0,\\
e^{kt}b(t)-b_0&=ka_0t,\\
b(t)&=e^{-kt}\left(b_0+ka_0t\right).
\end{aligned}
\tag{3}
$$

这组推导故意让每一行都包含不同长度的英文或数学 token，便于观察换行和对齐。总量守恒的检验写成

$$
M(t)=a(t)+b(t)+p(t),
\qquad
\frac{dM}{dt}=u(t),
\qquad
M(t)=M(0)+\int_0^t u(s)\,ds.
\tag{4}
$$

### 2.2 输入、采样与噪声

每个样本在 $t_k=k\Delta t$ 处采样，默认 $\Delta t=0.25$，$k=0,1,\ldots,63$。输入由一个慢正弦、一项衰减和一个局部脉冲组成：

$$
u(t)=u_0+u_1\sin(\omega t+\varphi)+u_2e^{-t/\tau}+u_3\exp\left[-\frac{(t-t_c)^2}{2\sigma^2}\right].
\tag{5}
$$

高斯项的归一化依据经典积分

$$
\int_{-\infty}^{\infty}e^{-x^2}\,dx=\sqrt{\pi},
\qquad
\int_{-\infty}^{\infty}\exp\left[-\frac{(t-t_c)^2}{2\sigma^2}\right]dt
=\sqrt{2\pi}\,\sigma.
\tag{6}
$$

观测值是确定性伪随机序列的加性扰动，写作

$$
z_k = h(x(t_k),m)+\epsilon_k,
\qquad
\epsilon_k\sim\mathcal N(0,\sigma_\epsilon^2),
\qquad
\mathbb E[\epsilon_k]=0,
\quad
\operatorname{Var}(\epsilon_k)=\sigma_\epsilon^2.
\tag{7}
$$

`seed = 1729` 只用于让验收者复现同一份 fixture；它不是密码，也不代表随机性研究结论。为覆盖滚动缓存的变动，另外生成 `seed = 2718` 和 `seed = 31415` 的两组扰动，参数只改变噪声幅度与脉冲中心，正文不把它们合并成一个虚假的大样本。

### 2.3 傅里叶表征

对长度为 $N$ 的离散序列 $x_0,\ldots,x_{N-1}$，使用非归一化正向变换和 $1/N$ 归一化逆变换：

$$
X_n=\sum_{k=0}^{N-1}x_k\exp\left(-2\pi i\frac{kn}{N}\right),
\qquad
x_k=\frac{1}{N}\sum_{n=0}^{N-1}X_n\exp\left(2\pi i\frac{kn}{N}\right).
\tag{8}
$$

连续版本与卷积关系分别为

$$
\widehat f(\xi)=\int_{-\infty}^{\infty}f(t)e^{-2\pi i\xi t}\,dt,
\qquad
f(t)=\int_{-\infty}^{\infty}\widehat f(\xi)e^{2\pi i\xi t}\,d\xi,
\tag{9}
$$

$$
\widehat{(f*g)}(\xi)=\widehat f(\xi)\widehat g(\xi),
\qquad
(f*g)(t)=\int_{-\infty}^{\infty}f(s)g(t-s)\,ds.
\tag{10}
$$

在复平面上，欧拉恒等式提供最短的旋转基元：

$$
e^{i\pi}+1=0,
\qquad
e^{i\omega t}=\cos(\omega t)+i\sin(\omega t).
\tag{11}
$$

式（11）同时检验 $i$、$\pi$、三角函数、加号与等号在窄行内的密度。本文不声称傅里叶特征一定优于时域特征；它们只是使验收输入包含复数和长求和的稳定方式。

### 2.4 分子描述符

每个 SMILES 源会被映射到一个合成描述符向量 $m\in\mathbb R^d$。为让数值可解释，本文只使用原子数、键数、芳香原子比例、形式电荷和同位素标记等字段；描述符的线性组合写成

$$
m=\begin{bmatrix}n_{\mathrm{atom}}&n_{\mathrm{bond}}&r_{\mathrm{arom}}&q_{\mathrm{formal}}&r_{\mathrm{iso}}\end{bmatrix}^{\mathsf T}
\in\mathbb R^5.
\tag{12}
$$

为了展示协方差和矩阵排版，中心化描述符的协方差采用

$$
\Sigma_m=\frac{1}{L-1}\sum_{j=1}^{L}(m_j-\bar m)(m_j-\bar m)^{\mathsf T},
\qquad
\bar m=\frac{1}{L}\sum_{j=1}^{L}m_j.
\tag{13}
$$

一个二维投影的特征向量满足

$$
\Sigma_m v_\ell=\lambda_\ell v_\ell,
\qquad
v_\ell^{\mathsf T}v_r=\delta_{\ell r},
\qquad
\lambda_1\geq\lambda_2\geq0.
\tag{14}
$$

这里的主成分只是视觉上方便的投影；它不等价于化学性质预测，也不替代结构验证。

### 2.5 注意力编码器

将动力学窗口、频域摘要和分子向量拼接成 token $y_k$ 后，使用模型维度 $d$ 的线性映射得到 query、key 和 value：

$$
Q=YW_Q,
\qquad
K=YW_K,
\qquad
V=YW_V,
\qquad
Y\in\mathbb R^{L\times d_{\mathrm{in}}}.
\tag{15}
$$

缩放点积注意力为

$$
\operatorname{Attn}(Q,K,V)
=\operatorname{softmax}\left(\frac{QK^{\mathsf T}}{\sqrt{d_k}}+M\right)V.
\tag{16}
$$

其中遮罩 $M_{ij}$ 在允许连接时为 $0$，在禁止未来信息泄露时为 $-\infty$ 的数值近似。softmax 的第 $i$ 行满足

$$
\alpha_{ij}=\frac{\exp(s_{ij})}{\sum_{r=1}^{L}\exp(s_{ir})},
\qquad
\sum_{j=1}^{L}\alpha_{ij}=1,
\qquad
0\leq\alpha_{ij}\leq1.
\tag{17}
$$

多头版本把不同投影的结果拼接，再经过输出矩阵：

$$
\operatorname{MultiHead}(Y)=\operatorname{Concat}(H_1,\ldots,H_h)W_O,
\qquad
H_r=\operatorname{Attn}(YW_Q^{(r)},YW_K^{(r)},YW_V^{(r)}).
\tag{18}
$$

正弦位置编码则为

$$
\begin{aligned}
P_{k,2r}&=\sin\left(k/10000^{2r/d}\right),\\
P_{k,2r+1}&=\cos\left(k/10000^{2r/d}\right),\\
Y'_k&=Y_k+P_k.
\end{aligned}
\tag{19}
$$

残差和 LayerNorm 的展示式为

$$
\operatorname{LN}(x)=\gamma\odot\frac{x-\mu_x\mathbf 1}{\sqrt{\sigma_x^2+\varepsilon}}+\beta,
\qquad
\operatorname{Block}(x)=x+\operatorname{FFN}(\operatorname{LN}(x)).
\tag{20}
$$

为防止把注意力热图误写成因果解释，我们只把它称为“合成权重可视化”。注意力行和为 1 是 softmax 的代数性质，不代表某个分子真的导致了某个代谢结果。

### 2.6 训练目标与概率记号

预测头输出 $\hat y_k$，以均方误差和轻微的 L2 正则为合成目标：

$$
\mathcal L(\theta)=\frac{1}{N}\sum_{k=1}^{N}(y_k-\hat y_k)^2+\lambda\lVert\theta\rVert_2^2,
\qquad
\lVert\theta\rVert_2=\sqrt{\sum_j\theta_j^2}.
\tag{21}
$$

梯度下降的一步更新为

$$
\theta_{r+1}=\theta_r-\eta\nabla_\theta\mathcal L(\theta_r),
\qquad
\nabla_\theta\mathcal L=\begin{bmatrix}\partial\mathcal L/\partial\theta_1&\cdots&\partial\mathcal L/\partial\theta_p\end{bmatrix}^{\mathsf T}.
\tag{22}
$$

若把目标视为条件概率，负对数似然可以写成

$$
\mathcal J(\theta)=-\sum_{k=1}^{N}\log p_\theta(y_k\mid y_{<k},m),
\qquad
p_\theta(y_k\mid y_{<k},m)\geq0,
\quad
\sum_{y_k}p_\theta(y_k\mid y_{<k},m)=1.
\tag{23}
$$

合成后验的贝叶斯记号为

$$
p(\theta\mid D)=\frac{p(D\mid\theta)p(\theta)}{p(D)},
\qquad
p(D)=\int p(D\mid\theta)p(\theta)\,d\theta.
\tag{24}
$$

KL 散度和熵只用于检验上下标、期望与求和：

$$
D_{\mathrm{KL}}(p\Vert q)=\sum_i p_i\log\frac{p_i}{q_i},
\qquad
H(p)=-\sum_i p_i\log p_i.
\tag{25}
$$

它们没有被拿来证明模型对真实分布有效。

## 3. 分子语料与化学边界

下列每个 fenced block 只有一行 SMILES 源；渲染器若支持结构预览，可以把源转换为二维结构；若仅支持文本，也必须保持源可复制。块与块之间的说明刻意改变长度，覆盖 short token、branch 和 ring closure 的混合。化学源不包含手性 `@`、`@@`、正斜杠或反斜杠；这些语法不在本文的“已支持结构”声明内。

### 3.1 芳香环、分支和常见小分子

苯是最小的芳香环夹具，环闭合数字 `1` 两次出现：

```smiles
c1ccccc1
```

吡啶把一个芳香碳替换为芳香氮，便于观察元素字母和环闭合的组合：

```smiles
n1ccccc1
```

阿司匹林同时包含芳香环、酯基和羧酸分支：

```smiles
CC(=O)Oc1ccccc1C(=O)O
```

咖啡因用于检验多个羰基、含氮环和分支的连续布局：

```smiles
Cn1c(=O)c2c(ncn2C)n(C)c1=O
```

尿嘧啶提供另一个含氮环，并保持短源长度：

```smiles
O=C1NC=CC(=O)N1
```

### 3.2 生物动力学中使用的小分子

甘氨酸不含环，却包含胺和羧酸两个常见官能团：

```smiles
NCC(=O)O
```

丙氨酸在中心碳上添加甲基分支：

```smiles
CC(N)C(=O)O
```

乳酸用于观察羟基与羧酸并列时的宽度变化：

```smiles
CC(O)C(=O)O
```

一个简化的葡萄糖环源只用常见的环和支链写法，不把该字符串当作立体化学断言：

```smiles
C(C1C(C(C(C(O1)O)O)O)O)O
```

### 3.3 离子与同位素

氯化钠测试两个带电片段和点分隔符：

```smiles
[Na+].[Cl-]
```

乙酸铵测试带电氮与羧酸根的组合：

```smiles
[NH4+].[O-]C(=O)C
```

甲烷的碳-13 版本只用于验证同位素方括号：

```smiles
[13CH4]
```

本节的源只构成渲染输入，不代表系统对每种键级、互变异构、价态或药理含义都已完成化学验证。原子数、键数和字节数应在解析入口处限制在本验收任务给出的边界内：单块最多 128 个原子、192 条键、2048 字节。

## 4. 推导：从守恒到预测

### 4.1 线性系统的离散化

将式（1）记作 $\dot x=Kx+Bu$，显式 Euler 离散化得到

$$
x_{k+1}=x_k+\Delta t(Kx_k+Bu_k)
=\left(I+\Delta tK\right)x_k+\Delta tBu_k.
\tag{26}
$$

当时间步长减小时，离散轨迹与合成参考轨迹的差异按一阶项缩小。为了让“尺度”在公式中可见，我们定义快、慢两套采样：

$$
\Delta t_{\mathrm{fast}}=0.125,
\qquad
\Delta t_{\mathrm{slow}}=0.5,
\qquad
N_{\mathrm{fast}}\Delta t_{\mathrm{fast}}=N_{\mathrm{slow}}\Delta t_{\mathrm{slow}}=16.
\tag{27}
$$

这两个窗口具有相同的总时长，却有不同的 token 数，因此能够让注意力矩阵的宽度和窄屏滚动长度产生真实变化。

### 4.2 传递函数与频率响应

对于单一衰减池 $\dot a=-ka+u$，零初值下的拉普拉斯域表达式为

$$
sA(s)=-kA(s)+U(s),
\qquad
G(s)=\frac{A(s)}{U(s)}=\frac{1}{s+k}.
\tag{28}
$$

将 $s=i\omega$ 代入，可得幅值和相位的合成参照：

$$
\lvert G(i\omega)\rvert=\frac{1}{\sqrt{k^2+\omega^2}},
\qquad
\arg G(i\omega)=-\arctan\left(\frac{\omega}{k}\right).
\tag{29}
$$

当 $\omega\to0$ 时，低频成分近似保留；当 $\omega\to\infty$ 时，高频输入被衰减。这里的极限只验证公式排版和量纲关系，不是对真实细胞系统的结论。

### 4.3 cases 与边界策略

为覆盖分段公式，合成的窗口权重定义为

$$
w(t)=
\begin{cases}
0, & t<0,\\
t/T, & 0\leq t<T,\\
1, & t\geq T.
\end{cases}
\tag{30}
$$

掩码分数在可见与不可见位置之间切换：

$$
S_{ij}=\begin{cases}
q_i^{\mathsf T}k_j/\sqrt{d_k}, & j\leq i,\\
-10^9, & j>i.
\end{cases}
\tag{31}
$$

在有限浮点实现中，$-10^9$ 是 $-\infty$ 的数值近似；文档不把它当作精确无穷。分段公式专门放在多行段落与表格附近，便于检查块级 margin。

### 4.4 三步对齐推导

把预测头写成带偏置的线性读出 $\hat y=Wh+b$，其平方损失的微分可以紧凑地写成：

$$
\begin{aligned}
\mathcal L &=\frac12\lVert Wh+b-y\rVert_2^2,\\
d\mathcal L&=(Wh+b-y)^{\mathsf T}(W\,dh+db),\\
\nabla_h\mathcal L&=W^{\mathsf T}(Wh+b-y),\\
\nabla_W\mathcal L&=(Wh+b-y)h^{\mathsf T}.
\end{aligned}
\tag{32}
$$

等号在每一行保留，`aligned` 中的对齐点位于等号附近。该推导的目的，是让验收器同时遇到范数、转置、梯度、矩阵乘法和长变量名。

## 5. 合成实验设计

### 5.1 数据生成协议

每个合成实验运行同一组步骤：先用式（1）生成参考轨迹，再按式（5）加入输入，采样后附加式（7）的确定性噪声；随后为每个 SMILES 计算 5 维描述符，按时间窗口拼接 token，最后将目标通量的下一步值作为预测标签。流程可以用下面的伪代码表达：

```python
def make_fixture(seed, dt, rate_ratio, pulse_center):
    time = arange(0.0, 16.0, dt)
    input_flux = synthetic_input(time, center=pulse_center)
    state = euler_chain(time, input_flux, k1=0.18 * rate_ratio, k2=0.18)
    noisy = observe(state, seed=seed, sigma=0.015 + 0.002 * rate_ratio)
    molecule = smiles_descriptors(SMILES_FIXTURE)
    tokens = join_time_frequency_and_molecule(noisy, molecule)
    return tokens[:-1], noisy[1:, 2]
```

代码是文档中的静态示例；本次写作没有启动 AI 服务，也没有执行昂贵的训练任务。`synthetic_input`、`euler_chain` 等名称用于让阅读者理解数据流，不承诺仓库中存在同名运行时函数。

### 5.2 预注册参数

表 1 预先列出四个小实验。`ratio` 是 $\rho$，`width` 是每个 token 的综合维度；不同值用于让长段落中出现可观察的数值变化。

| 组别 | seed | $\Delta t$ | ratio $\rho$ | pulse center $t_c$ | width | 目的 |
|---|---:|---:|---:|---:|---:|---|
| A / baseline | 1729 | 0.25 | 1.00 | 6.0 | 32 | 基线结构、短表格和常规注意力 |
| B / fast | 2718 | 0.125 | 0.75 | 5.5 | 48 | 加倍时间 token，观察窄窗口换行 |
| C / slow | 31415 | 0.50 | 1.50 | 7.0 | 24 | 减少 token，观察缓存重新布局 |
| D / aromatic | 1618 | 0.25 | 2.00 | 8.25 | 40 | 增加芳香分子描述符，观察表格宽度 |

输入幅度参数固定为 $u_0=0.20$、$u_1=0.05$、$u_2=0.08$、$u_3=0.12$，但 $\tau$、$\omega$ 与 $\sigma$ 按组别变化：

$$
\begin{aligned}
(\tau_A,\omega_A,\sigma_A)&=(4.0,0.80,0.60),\\
(\tau_B,\omega_B,\sigma_B)&=(3.0,1.20,0.45),\\
(\tau_C,\omega_C,\sigma_C)&=(6.0,0.55,0.90),\\
(\tau_D,\omega_D,\sigma_D)&=(2.5,1.60,0.35).
\end{aligned}
\tag{33}
$$

### 5.3 评价量

误差表只使用合成标签。给定预测值 $\hat y_i$ 和目标 $y_i$，评价量定义为

$$
\operatorname{MAE}=\frac1N\sum_{i=1}^{N}\lvert y_i-\hat y_i\rvert,
\qquad
\operatorname{RMSE}=\sqrt{\frac1N\sum_{i=1}^{N}(y_i-\hat y_i)^2}.
\tag{34}
$$

为展示相关性和决定系数的排版，再定义

$$
r=\frac{\sum_i(y_i-\bar y)(\hat y_i-\bar{\hat y})}{\sqrt{\sum_i(y_i-\bar y)^2}\sqrt{\sum_i(\hat y_i-\bar{\hat y})^2}},
\qquad
R^2=1-\frac{\sum_i(y_i-\hat y_i)^2}{\sum_i(y_i-\bar y)^2}.
\tag{35}
$$

每个区间由固定的 200 次索引重采样得到：

$$
\operatorname{CI}_{0.90}(T)=\left[q_{0.05}\left(T^{*}\right),q_{0.95}\left(T^{*}\right)\right],
\qquad
T^{*}=T(y_{i_1},\ldots,y_{i_N}),
\quad i_j\sim\operatorname{Uniform}\{1,\ldots,N\}.
\tag{36}
$$

由于样本和重采样次数很小，区间只是视觉夹具中的数字，不应被当作统计推断。

### 5.4 分子清单与结构标签

表 2 把上一节的源码和合成标签放在一起。标签是人为定义的 fixture class，不是化合物的临床、毒理或药理分类。

| ID | 源摘要 | `aromatic` | `charged` | `isotope` | fixture class |
|---|---|---:|---:|---:|---|
| M01 | `c1ccccc1` | 1 | 0 | 0 | ring |
| M02 | `n1ccccc1` | 1 | 0 | 0 | hetero-ring |
| M03 | aspirin source | 1 | 0 | 0 | branch |
| M04 | caffeine source | 1 | 0 | 0 | multi-carbonyl |
| M05 | glycine source | 0 | 0 | 0 | amino-acid |
| M06 | `[Na+].[Cl-]` | 0 | 1 | 0 | ionic |
| M07 | `[13CH4]` | 0 | 0 | 1 | isotope |

表 2 的反引号内容只作源代码标签；真正完整的 SMILES 一行位于第 3 节 fenced block 中。这样可同时测试 inline code 和多行 fenced code 的视觉差异。

### 5.5 参考图片

下面的本地图片是 96 × 48 像素的小型科学夹具。它只用于验证 Markdown 图片的加载、尺寸占位、缺失回退和滚动位置；颜色与像素没有科研含义。

![96 × 48 像素的本地科学夹具图](screenshots/scientific-fixture.png)

图 1 的 alt text 同时包含中文、数字和 multiplication sign；实际渲染若找不到文件，应保留 alt text 或可读回退，而不应让后续公式的布局消失。

## 6. 合成实验结果

### 6.1 轨迹与守恒检查

表 3 给出由协议预注册的参考统计。`mass drift` 是离散总量与输入积分之间的最大绝对差；它是数值构造中的诊断量，不是实验测量。小数位故意不统一，以测试表格列宽与中文列名。

| 组别 | $\max_t a(t)$ | $\max_t b(t)$ | $p(16)$ | mass drift | 备注 |
|---|---:|---:|---:|---:|---|
| A | 1.000 | 0.238 | 2.731 | $3.1\times10^{-4}$ | baseline |
| B | 1.000 | 0.241 | 2.728 | $1.7\times10^{-4}$ | fast sampling |
| C | 1.000 | 0.231 | 2.742 | $6.8\times10^{-4}$ | slow sampling |
| D | 1.000 | 0.255 | 2.719 | $3.2\times10^{-4}$ | aromatic descriptor |

离散总量的核对写成

$$
\Delta_M=\max_{0\leq k\leq N}\left\lvert M_k-M_0-\Delta t\sum_{j=0}^{k-1}u_j\right\rvert,
\qquad
\Delta_M\geq0.
\tag{37}
$$

表 3 的数值只为这条定义服务。它们不会被推广为 Euler 方法的普遍误差界。

### 6.2 预测统计

四组预测头使用同一个确定性线性基线，再叠加由式（16）生成的合成权重。表 4 的数值作为渲染输入预先固定；如果实现重新计算得到不同数字，应以实现日志为准并在验收记录中注明，而不是把本文数字改写成“已达标”。

| 组别 | MAE | RMSE | $R^2$ | $r$ | 90% synthetic interval for RMSE |
|---|---:|---:|---:|---:|---|
| A | 0.031 | 0.040 | 0.912 | 0.958 | [0.037, 0.044] |
| B | 0.028 | 0.036 | 0.928 | 0.964 | [0.033, 0.040] |
| C | 0.043 | 0.055 | 0.841 | 0.919 | [0.049, 0.062] |
| D | 0.034 | 0.045 | 0.887 | 0.944 | [0.041, 0.050] |

以组 A 为例，四舍五入前的合成误差使用

$$
\operatorname{RMSE}_A=\sqrt{\frac{1}{64}\sum_{k=1}^{64}(y_k-\hat y_k)^2}=0.0402,
\qquad
\operatorname{MAE}_A=0.0314.
\tag{38}
$$

组 C 的较大误差来自更少的采样点与较宽的脉冲，而不是一个经过真实训练的模型退化。为了避免读者误把表格当作 benchmark，本文没有报告 GPU 时间、吞吐或多用户并发率。

### 6.3 注意力矩阵的局部切片

组 B 使用 $L=128$、$d_k=8$，因此单个 head 的分数矩阵属于 $\mathbb R^{128\times128}$。以下是取前三个 query、前五个 key 的小切片；数值四舍五入，行和只在完整行上为 1：

$$
A_{0:3,0:5}=\begin{bmatrix}
0.31&0.24&0.18&0.15&0.12\\
0.10&0.29&0.25&0.20&0.16\\
0.07&0.11&0.34&0.27&0.21
\end{bmatrix}.
\tag{39}
$$

局部数字的和不必等于 1，因为它省略了其余 key。完整行的代数性质仍是

$$
\mathbf 1^{\mathsf T}\alpha_i=1,
\qquad
\alpha_i\in\Delta^{L-1}=\left\{v\in\mathbb R^L:v_j\geq0,\ \sum_{j=1}^{L}v_j=1\right\}.
\tag{40}
$$

该段落同时测试矩阵括号、集合条件、\Delta 符号、范数空间和长中文解释是否互相挤压。

### 6.4 跨尺度扰动与消融

为了让不同窗口长度真实地产生不同的语料形状，我们对同一条参考轨迹构造三种时间聚合。令 $q$ 为正整数，区间平均和端点抽样分别为

$$
\bar{x}^{(q)}_j=\frac{1}{q}\sum_{r=0}^{q-1}x_{qj+r},
\qquad
x^{(q)}_j=x_{qj+q-1},
\qquad
j=0,\ldots,\left\lfloor\frac{N}{q}\right\rfloor-1.
\tag{41}
$$

组 A 采用 $q=1$，组 B 在频域摘要前采用 $q=2$，组 C 采用 $q=4$，组 D 采用 $q=1$ 但移除芳香比例字段。这样，公式、表格与 token 长度的变化来自参数含义，而不是复制段落。时间聚合的低通直觉可以用 sinc 形式表示：

$$
H_q(\omega)=\frac{1}{q}\sum_{r=0}^{q-1}e^{-i\omega r}
=e^{-i\omega(q-1)/2}\frac{\sin(q\omega/2)}{q\sin(\omega/2)}.
\tag{42}
$$

当分母接近零时，右侧按连续极限解释；这个边界提醒有意保留在长公式中，供渲染器检查括号和分数线。若采样频率为 $f_s$，合成输入的 Nyquist 频率定义为

$$
f_{\mathrm{Nyq}}=\frac{f_s}{2},
\qquad
\omega_{\mathrm{Nyq}}=\pi f_s,
\qquad
f_s=\frac{1}{\Delta t}.
\tag{43}
$$

我们再对注意力行的尖锐程度计算熵。它只用于产生一列解释性数值，不是信息论意义上的生物复杂度：

$$
\mathcal H_i=-\sum_{j=1}^{L}\alpha_{ij}\log(\alpha_{ij}+\varepsilon),
\qquad
0\leq\mathcal H_i\leq\log L+\delta_\varepsilon.
\tag{44}
$$

为比较“去掉分子向量”和“保留分子向量”两种合成输入，读出层对输入的局部敏感度使用 Jacobian：

$$
J_{m}(y)=\frac{\partial\hat y}{\partial m}
=\begin{bmatrix}
\partial\hat y_1/\partial m_1&\cdots&\partial\hat y_1/\partial m_5\\
\vdots&\ddots&\vdots\\
\partial\hat y_N/\partial m_1&\cdots&\partial\hat y_N/\partial m_5
\end{bmatrix}.
\tag{45}
$$

表 6 给出一个预注册的消融记录。数值是由文档夹具构造的相对量，符号 “—” 表示该列在这个版本中没有生成，不应被解释为缺失科研数据。

| 输入变体 | 时间聚合 $q$ | 是否含 $r_{\mathrm{arom}}$ | $L$ | mean attention entropy | 合成 MAE |
|---|---:|---:|---:|---:|---:|
| full / A | 1 | yes | 64 | 3.71 | 0.031 |
| pooled / B | 2 | yes | 64 | 3.42 | 0.028 |
| coarse / C | 4 | yes | 32 | 2.86 | 0.043 |
| no-aromatic / D | 1 | no | 64 | 3.55 | 0.034 |

表 6 的 token 长度取决于拼接规则，不等于真实 Transformer 的序列上限。特别地，熵减小并不证明模型找到“更重要”的分子片段；它只表示合成 softmax 行更集中。为了让该解释保持可审计，权重仍从式（17）定义的行归一化得到。

动力学部分还使用一个无量纲比值描述输入变化相对反应变化的大小：

$$
\mathrm{Da}=\frac{k_1T}{1+k_2T},
\qquad
\mathrm{Da}_A=0.58,
\quad
\mathrm{Da}_B=0.44,
\quad
\mathrm{Da}_C=0.71.
\tag{46}
$$

这里的 Damköhler-like 名称只表示构造出来的比例，不暗示本文覆盖任何具体反应工程制度。线性化的局部传播可以用矩阵指数表示：

$$
x(t+\delta t)=e^{K\delta t}x(t)+\int_0^{\delta t}e^{K(\delta t-s)}B\,u(t+s)\,ds,
\qquad
e^{K\delta t}=\sum_{r=0}^{\infty}\frac{(K\delta t)^r}{r!}.
\tag{47}
$$

当窗口从 $16$ 缩到 $8$、脉冲中心从 $6.0$ 移到 $8.25$ 时，式（47）中的积分上限和输入位置同步变化；因此验收者可以在同一份文档中观察参数、公式和解释段落的局部差异。我们没有把这些消融数值与表 4 的预测误差合并成单一排行榜，也没有选择一个“最佳”设置来宣称模型已经达标。

### 6.5 复现记录与人工核对

合成夹具的最小复现记录包含四类字段：参数、源、渲染环境和观察结果。参数字段至少应记录 seed、$\Delta t$、$\rho$、$t_c$、$\sigma$ 和 token width；源字段应记录一行 SMILES、数学块的原始定界符和图片相对路径；环境字段应记录字体、主题、viewport、tab 生命周期；观察字段则记录可见文本、回退行为和复制结果。

为防止“看起来相同”掩盖源文本差异，数学源的检查同时保留字符级等号和行分隔符。对于一个四行推导，行数计数可以写作

$$
n_{\mathrm{line}}=\sum_{\ell=1}^{4}\mathbf 1\!\left[\text{line}_{\ell}\text{ contains }=\right]=4,
\qquad
n_{\mathrm{break}}=3.
\tag{48}
$$

这里的指示函数只是文档内的审计记号；它不要求渲染器执行自然语言。SMILES 的源审计也只计数语料边界：

$$
b_{\mathrm{smiles}}=\operatorname{bytes}(s),
\qquad
n_{\mathrm{atom}}\leq128,
\qquad
n_{\mathrm{bond}}\leq192,
\qquad
b_{\mathrm{smiles}}\leq2048.
\tag{49}
$$

式（49）的不等式是验收输入约束，不是化学软件的完整结构合法性证明。若解析器拒绝某个有效源，记录应包含原始源和回退文本；若解析器接受 B.2 中的坏例，也应记录“环境额外接受”，不能修改正文的能力边界。

### 6.6 窗口敏感性记录

同一个内容块在宽窗口和窄窗口中承担的排版压力不同。为使观察可复现，我们预先选择三个 viewport 宽度 $w_1=1440$、$w_2=960$ 和 $w_3=640$（单位为 CSS px），并在每个宽度上阅读式（32）、表 4、咖啡因源和图 1。宽度本身不是性能分数，只是触发布局变化的输入。

$$
\kappa(w)=\frac{\text{visible content width at }w}{\text{viewport width }w},
\qquad
0<\kappa(w)\leq1.
\tag{50}
$$

若长公式保持完整，或者在允许的横向容器中滚动，均可作为可读行为记录；若公式覆盖相邻正文、表头被裁切且没有可访问回退，则记录为待修复布局问题。为了避免把主观印象写成数值结果，验收者同时记录截图、原始宽度、字体和滚动位置。

图片和结构源的回退也使用离散状态而非虚构的成功率：

$$
r_{\mathrm{asset}}=
\begin{cases}
0, & \text{asset is rendered or source remains readable},\\
1, & \text{asset is missing and no readable fallback remains}.
\end{cases}
\tag{51}
$$

这个状态变量只描述一个具体文档位置。它不代表全产品图片成功率，也不把当前本地 fixture 的存在推断成网络图片能力。缺图时，alt text “96 × 48 像素的本地科学夹具图”应仍然能帮助读者定位内容。

对于 tab 返回，记录返回前后的锚点差异：

$$
\Delta_{\mathrm{anchor}}=\left\lvert y_{\mathrm{return}}-y_{\mathrm{before}}\right\rvert,
\qquad
\Delta_{\mathrm{anchor}}=0\ \text{means exact scroll restoration in this fixture}.
\tag{52}
$$

实际产品可能采用可见段落、最近标题或其他合理锚点；因此式（52）只是建议的观察量，不是强制的像素级合同。表 7 记录本文件希望看到的状态描述：

| 视口 | 长公式 | 表格 | SMILES 源 | 图片 | 记录重点 |
|---:|---|---|---|---|---|
| 1440 px | 一行或自然留白 | 全列可见 | 源与结构均可复制 | 占位稳定 | 基线 |
| 960 px | 允许局部换行 | 重点列仍可读 | 不截断单行源 | alt text 可定位 | 中等宽度 |
| 640 px | 容器内滚动或清楚换行 | 不遮挡相邻段落 | 横向滚动不改源 | 缺图回退可见 | 窄窗口 |

表 7 是预期观察表，不是已经执行的截图报告。若一个实现选择不同的合法排版策略，复核者应记录策略及其可访问性，而不是为了匹配表格文字强行修改实现。

## 7. 讨论

### 7.1 这套夹具覆盖什么

从渲染角度看，本文把以下类型安排在同一条阅读路径上：

- short inline math：$e^{i\pi}+1=0$、$\rho=k_1/k_2$、$\lVert x\rVert_2$；
- long display math：含积分、矩阵、概率、softmax 和多行 `aligned` 的块；
- chemistry source：ring closure、branch、aromatic element、charged fragment、isotope；
- document primitives：表格、图片、代码块、引用编号、列表以及中英文混排；
- lifecycle perturbation：不同 token 长度、表格宽度、图片占位和多 tab 返回。

其中，长度变化是真实的语料变化：组 B 有 128 个时间 token，组 C 只有 32 个时间 token；式（33）又改变了指数衰减和脉冲宽度。它们不是复制同一段文字来填充页面。

### 7.2 解释边界

注意力权重可以被可视化，却不自动成为因果证据。分子描述符是五个合成字段，也不等同于完整的分子图网络。高斯积分、Euler 迭代和 softmax 恒等式都是数学定义或已知关系；把它们放在一个实验叙事中，是为了测试排版的组合复杂度。

如果将来把该夹具接入真实数据，应重新审查采样频率、量纲、缺失值、单位换算、训练和评估切分，并由领域专家检查化学结构。真实数据的伦理、隐私、许可证和可重复性不能由 Markdown 渲染器代替。

### 7.3 可访问性与可读性

图片使用描述性 alt text，表格第一行提供列名，代码和 SMILES 保留可复制源。数学块的相邻正文解释符号，不把颜色作为唯一语义。窄窗口下应允许长公式横向滚动或合理换行；若浏览器无法显示结构图，则源码回退必须仍然可见。

中英文混排有意保留：`synthetic fixture`、`baseline`、`source fallback`、`Transformer attention` 与中文解释相邻。验收时应观察字体回退、标点宽度、下标位置和代码等宽字体是否改变行高。

### 7.4 语料维护原则

这份语料以后若需要增加内容，应优先加入一个新的可观察边界，而不是复制一整节。例如，可以加入一个不同长度的 cases，一个含同位素的单行源，或一个缺图回退；每次新增都应说明它覆盖的解析器路径和预期观察。若只增加同样的句子来提高字节数，滚动缓存和布局扰动并不会因此更有代表性。

数学符号也应保持单一含义。在本文中，$L$ 表示 token 数，$d$ 表示特征宽度，$k_1,k_2$ 表示合成速率常数，$\lambda$ 表示正则化权重或特征值时由下标区分上下文；引用了不同领域的记号时，邻近正文会重新说明。化学源则按块独立解释，避免从一个分子块把键或电荷状态“借”到下一个块。

对比渲染结果时，复核者应优先保存原始 Markdown 和环境信息，再描述视觉差异。单张截图可能无法证明滚动往返后的缓存正确，单次复制也无法证明跨文字选区始终稳定。因此附录 C 把动作序列作为观察对象，并允许状态保持“待实机”。这是一份诚实的验收输入，而不是一张预填的通过清单。

## 8. 相关工作与引用

本文只借用公开教科书和论文中的常见记号，并没有复现引用工作的数据集或性能。引用用于验证参考文献布局、链接文本和中英文段落的组合；它们不构成对本文合成结果的外部背书。

经典动力学和统计物理的教材给出常微分方程、线性系统和随机变量的基础记号 [1]。傅里叶分析的标准定义与卷积关系见 [2]。Transformer 的 scaled dot-product attention 记号沿用 [3]，而 SMILES 的线性表示背景可参见 [4]。这些来源支持符号选择，不支持本文虚构的表 3 和表 4。

1. [1] H. Haken, *Synergetics: An Introduction*, Springer, 1983. 这里只作为动力学符号的教材式参照。
2. [2] R. N. Bracewell, *The Fourier Transform and Its Applications*, 3rd ed., McGraw-Hill, 2000. 这里只作为傅里叶定义的参照。
3. [3] A. Vaswani et al., “Attention Is All You Need,” *NeurIPS*, 2017. 本文只采用其常见注意力公式的记号。
4. [4] D. Weininger, “SMILES, a Chemical Language and Information System,” *J. Chem. Inf. Comput. Sci.*, 1988. 本文仅以短 SMILES 作为渲染源。

## 9. 结论

本文构造了一个独立的中文科研渲染验收语料：守恒动力学提供上下标、导数、积分与矩阵；傅里叶段落提供复数、无穷积分和求和；注意力段落提供 softmax、遮罩、多头、位置编码与概率统计；分子段落提供常见 SMILES 结构；实验段落提供表格、代码、图片和可变参数。所有数值都是合成的，文档本身不宣称真实科研结果，也不宣称 Nebula 已经通过大规模负载或全部渲染能力验收。

真正的产品验收还需要在目标构建、目标字体、目标窗口尺寸和实际 tab 生命周期上记录观察。附录中的空白或“待实机”状态应由主代理在运行渲染器后填写；没有运行证据时，不应把它们改成“通过”。

## 附录 A：公式索引与覆盖矩阵

为便于测试脚本或人工审阅定位，表 5 将主要公式映射到覆盖点。索引编号是本文内部编号，不是引用编号。

| 公式 | 主要覆盖 | 公式 | 主要覆盖 |
|---|---|---|---|
| (1) | ODE、矩阵、守恒 | (2) | 指数、极限、比例 |
| (3) | `aligned`、积分因子 | (4) | 积分、总量 |
| (5) | 正弦、高斯、参数 | (6) | 高斯积分、根号 |
| (7) | 概率、期望、方差 | (8) | DFT、复指数、求和 |
| (9) | 傅里叶积分 | (10) | 卷积 |
| (11) | 欧拉恒等式 | (12) | 向量、转置 |
| (13) | 协方差、均值 | (14) | 特征值、Kronecker delta |
| (15) | Q/K/V 矩阵 | (16) | scaled attention |
| (17) | softmax、单纯形 | (18) | multi-head |
| (19) | aligned、多行编码 | (20) | LayerNorm、残差 |
| (21) | MSE、L2 范数 | (22) | 梯度更新 |
| (23) | 条件概率 | (24) | Bayes 后验、积分 |
| (25) | KL、熵 | (26) | Euler 离散化 |
| (27) | 多尺度时间步 | (28) | Laplace、传递函数 |
| (29) | 幅值、相位 | (30) | `cases` |
| (31) | 因果 mask、`cases` | (32) | 多行梯度推导 |
| (33) | 参数表、aligned | (34) | MAE、RMSE |
| (35) | 相关性、$R^2$ | (36) | bootstrap 区间 |
| (37) | 离散守恒 | (38) | 长等式、数值 |
| (39) | 矩阵切片 | (40) | 概率单纯形 |

此外，本文在各段正文中使用了短公式，例如 $A\to B\to P$、$s=i\omega$、$\alpha_{ij}\geq0$、$L\times d$、$\mathbb R^5$、$\delta_{\ell r}$ 和 $\varepsilon>0$。它们与块级公式相邻，用于检查同一段中的 baseline prose 与 inline math。

## 附录 B：渲染能力边界与源码回退

### B.1 明确的未来能力对照

下列字符串只作为未来能力或源码对照，不应被当前渲染器当作已经支持的结构图。`\ce`、`\pu`、`\SI` 依赖额外宏包或专门解析器；Mermaid、FASTA、Newick 和 PDB 也需要各自的语言支持。为了保持验收语义清楚，它们放在 fenced code 中：

```tex
\ce{A -> B + C}
\pu{3.0e8 m s-1}
\SI{25}{\celsius}
```

```mermaid
flowchart LR
    A[substrate] --> B[intermediate]
    B --> C[product]
```

```fasta
>synthetic-sequence
ACGTACGTACGT
```

```text
((A:0.1,B:0.2):0.3,C:0.4);
ATOM      1  N   GLY A   1      11.104  13.207   9.451
```

这些代码块应保持源码可读；除非产品另有明确渲染器，不应在验收记录中写“已变图”。

### B.2 无效或不在支持范围的坏例

下面的两个块专门标记为 source fallback 负例。它们不属于正文中的有效 SMILES 或数学示例。第一个包含手性标记，第二个包含未配置的 `\ce` 命令；期望行为是显示原始源码、错误标记或明确的降级文本，而不是悄悄生成一个错误结构。

<!-- pebrel-test: source-fallback -->
```smiles
C[C@H](O)C(=O)O
```

<!-- pebrel-test: source-fallback -->
$$
\ce{H2O + CO2 -> H2CO3}
$$

正文有效源没有使用这些语法。若某个运行环境恰好支持它们，也只能在实机观察中记录“该环境额外支持”，不能反推本项目的通用合同已经扩大。

## 附录 C：交互验收观察表

本附录是待填写的验收协议。它列出动作、观察点、证据和状态；当前文档生成过程没有启动 GUI、AI 服务或外部网络服务，因此状态统一为“待实机”。主代理可以在实际渲染器中逐项记录，而不把静态 Markdown 检查冒充为交互通过。

| 编号 | 操作 | 观察点 | 证据建议 | 当前状态 |
|---|---|---|---|---|
| C1 | 从摘要滚动到式（1）与式（3） | 上下标、矩阵行和 `aligned` 行距不重叠 | 记录窗口宽度与截图 | 待实机 |
| C2 | 从式（3）滚动到式（11） | 长公式、欧拉恒等式和中文段落连续可读 | 记录窄窗口与宽窗口 | 待实机 |
| C3 | 从 SMILES M01 拖选到 M04 | 跨文字选区不吞掉代码围栏或数学块 | 记录选区起止文本 | 待实机 |
| C4 | 在表 1、式（33）、表 4 间往返 | 表格列宽和公式缓存随滚动保持稳定 | 记录第一次与第二次布局 | 待实机 |
| C5 | 调整窗口宽度并 resize | `aligned`、`cases`、矩阵和中文英文混排重新布局 | 记录至少 3 个宽度 | 待实机 |
| C6 | 切换到另一个 tab 后返回本文 | 公式、图片占位、代码块和滚动锚点仍存在 | 记录返回后的首屏 | 待实机 |
| C7 | 暂时移除 `scientific-fixture.png` | alt text 或缺图回退可见，后续内容不坍塌 | 记录缺失图片截图 | 待实机 |
| C8 | 检查坏例 B.2 | 手性 SMILES 与 `\ce` 不被错误宣称为有效结构 | 记录源码回退文本 | 待实机 |
| C9 | 检查 Mermaid、FASTA、Newick、PDB | 未配置渲染器时仍显示源码 | 记录 fenced code 行为 | 待实机 |
| C10 | 从附录返回摘要再到表 4 | 长文缓存、滚动位置和重复参数无错位 | 记录往返序列 | 待实机 |
| C11 | 复制式（1）、式（16）、SMILES M03 | 复制文本保留等号、反斜杠语法和一行源 | 记录剪贴板文本 | 待实机 |
| C12 | 在暗色与亮色主题间切换（若产品提供） | 公式、表格边框、代码和图片对比度可读 | 记录主题与字体 | 待实机 |

### C.1 负载范围说明

本文是单份中等大小的静态语料，目标是检查科学语法与交互边界。它不测试 80 个真实 AI 会话，不测试 100 MB 文档，不测试多机同步、远端模型排队或长时间 GPU 占用。那些场景需要有预算的独立验证：单独的负载模型、资源上限、超时策略、取消语义和可观测性记录都应先定义，再决定是否运行。任何人都不能仅凭本文“渲染成功”写出“系统承受 80 AI”或“系统已处理 100 MB”的结论。

### C.2 实机记录模板

```text
Renderer build:
Host OS and display scale:
Viewport sizes:
Font configuration:
Tab sequence:
Observed formula issues:
Observed SMILES behavior:
Missing-image behavior:
Selection and copy behavior:
Open syntax or layout issues:
Evidence paths:
Reviewer and date:
```

## 附录 D：复核者速查

复核者可以按以下顺序快速检查语料，而不需要先运行完整产品：

1. 搜索 `$$`，确认块级数学成对出现，并检查多行块确实含有两个反斜杠的行分隔符。
2. 搜索 ` ```smiles`，逐块确认下一行只有一条源码，并检查正文有效源不含 `@`、`@@`、正斜杠或反斜杠。
3. 搜索 `<!-- pebrel-test: source-fallback -->`，确认每个注释都紧接数学块或 SMILES 块。
4. 检查表 1、表 2、表 3、表 4 和表 5 的表头、长单元格与中文英文混排。
5. 检查图片路径 `screenshots/scientific-fixture.png` 是否相对当前 Markdown 文件正确。
6. 记录未决语法，不把源码回退、缺图或未运行的交互项目改写成成功结果。

本文到此结束。最后一次强调：它是一份方法学与渲染验收示例，数据是 synthetic，结论只涉及语料覆盖和待观察的交互协议；它不是现实生物医学研究，也不是产品容量证明。
