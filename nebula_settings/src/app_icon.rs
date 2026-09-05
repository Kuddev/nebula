#[derive(Copy, Clone, Debug)]
pub struct AppIconPalette {
    pub key: &'static str,
    pub number: &'static str,
    pub name_zh: &'static str,
    pub name_en: &'static str,
    pub tile: u32,
    pub mark: u32,
    pub border: u32,
}

macro_rules! app_icons {
    ($default:ident => ($key:literal, $number:literal, $zh:literal, $en:literal, $tile:literal, $mark:literal, $border:literal),
     $($variant:ident => ($variant_key:literal, $variant_number:literal, $variant_zh:literal, $variant_en:literal, $variant_tile:literal, $variant_mark:literal, $variant_border:literal)),+ $(,)?) => {
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default)]
        pub enum AppIconName {
            #[default]
            $default,
            $($variant),+
        }

        impl AppIconName {
            pub const ALL: [Self; 25] = [Self::$default, $(Self::$variant),+];

            pub fn from_settings(value: &str) -> Option<Self> {
                Self::ALL.into_iter().find(|variant| variant.settings_value().eq_ignore_ascii_case(value.trim()))
            }

            pub const fn settings_value(self) -> &'static str {
                self.palette().key
            }

            pub const fn palette(self) -> AppIconPalette {
                match self {
                    Self::$default => AppIconPalette {
                        key: $key, number: $number, name_zh: $zh, name_en: $en,
                        tile: $tile, mark: $mark, border: $border,
                    },
                    $(Self::$variant => AppIconPalette {
                        key: $variant_key, number: $variant_number, name_zh: $variant_zh, name_en: $variant_en,
                        tile: $variant_tile, mark: $variant_mark, border: $variant_border,
                    }),+
                }
            }
        }
    };
}

app_icons! {
    Titanium => ("titanium", "21", "钛银", "Titanium", 0xD9DFE6, 0x374151, 0x909BA9),
    Glacier => ("glacier", "01", "冰川白", "Glacier", 0xEEF4FC, 0x234B85, 0x93AEC9),
    IceBlue => ("ice-blue", "02", "冰蓝雾", "Ice blue", 0xDCEBFF, 0x2353BA, 0x839DC8),
    Cobalt => ("cobalt", "03", "钴蓝", "Cobalt", 0x275DDF, 0xF3F7FF, 0x1C45AC),
    MidnightBlue => ("midnight-blue", "04", "午夜蓝", "Midnight blue", 0x101D3A, 0xDBEAFE, 0x647DAA),
    SteelBlue => ("steel-blue", "05", "钢蓝", "Steel blue", 0xCBD8E8, 0x334A68, 0x879BAF),
    GlassBlue => ("glass-blue", "06", "琉璃蓝", "Glass blue", 0x4668CF, 0xF5F7FF, 0x3152B2),
    PorcelainBlue => ("porcelain-blue", "07", "蓝瓷", "Porcelain blue", 0xF5F7FC, 0x1D4ED8, 0xADBBD7),
    DeepBlue => ("deep-blue", "08", "深海电蓝", "Deep blue", 0x101827, 0x8BB9FF, 0x5578A6),
    MistViolet => ("mist-violet", "09", "雾紫", "Mist violet", 0xEDE9FE, 0x6640B4, 0xAD9BCD),
    Violet => ("violet", "10", "紫罗兰", "Violet", 0x6C47C9, 0xF6F0FF, 0x5235A2),
    NightViolet => ("night-violet", "11", "冷夜紫", "Night violet", 0x251D3C, 0xD9C7FF, 0x78668F),
    SilverViolet => ("silver-violet", "12", "银紫", "Silver violet", 0xE6E7F4, 0x585B89, 0xAAACC3),
    IceCyan => ("ice-cyan", "13", "冰青", "Ice cyan", 0xDEF6FA, 0x0C657A, 0x79ABB7),
    Cyan => ("cyan", "14", "青蓝", "Cyan", 0x087E9A, 0xECFDFF, 0x085E76),
    PolarCyan => ("polar-cyan", "15", "极夜青", "Polar cyan", 0x102B36, 0x9FE5EE, 0x4D8B97),
    CoolMint => ("cool-mint", "16", "冷薄荷", "Cool mint", 0xDAF5E9, 0x17644E, 0x81B9A4),
    Emerald => ("emerald", "17", "冷翡翠", "Emerald", 0x116953, 0xECFFF5, 0x0B4C3E),
    NightMint => ("night-mint", "18", "夜薄荷", "Night mint", 0x112A28, 0xA5EDD6, 0x527F76),
    Porcelain => ("porcelain", "19", "冷瓷白", "Porcelain", 0xF4F5F7, 0x252A33, 0xA5ACB7),
    Graphite => ("graphite", "20", "石墨", "Graphite", 0x232730, 0xF1F3F7, 0x717984),
    Monochrome => ("monochrome", "22", "纯黑白", "Monochrome", 0x111318, 0xFFFFFF, 0x737982),
    SageLight => ("sage-light", "23", "原版浅苔", "Original light sage", 0xE4EBDD, 0x263A32, 0x819581),
    SageDark => ("sage-dark", "24", "原版深苔", "Original dark sage", 0x18231F, 0xE8EFE2, 0x819581),
    GraphiteViolet => ("graphite-violet", "25", "石墨紫", "Graphite violet", 0x25262D, 0xCFD0E8, 0x777986),
}
