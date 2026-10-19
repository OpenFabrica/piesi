//! 批量准确率回归：按语言、按名字长度统计识别正确率与 ambiguous 率，给调参一个不会悄悄退化的底线。
//! 样本是常见的文件名用词（2 字到 8 字），每个名字单独成包、带扩展名。

use piesi_core::encoding::{detect_raw, Hints, NameEncoding};

/// 用指定编码把字符串转成原始字节。
fn enc(s: &str, e: NameEncoding) -> Vec<u8> {
    let (bytes, _, had_errors) = e.encoding().encode(s);
    assert!(!had_errors, "{s} 无法用 {} 编码", e.label());
    bytes.into_owned()
}

/// 一种语言的样本：编码、本地 locale、名字（不含扩展名）。
struct Sample {
    tag: &'static str,
    encoding: NameEncoding,
    locale: &'static str,
    names: &'static [&'static str],
}

const SAMPLES: &[Sample] = &[
    Sample {
        tag: "GBK",
        encoding: NameEncoding::Gb18030,
        locale: "zh-CN",
        names: &[
            "报告", "文档", "照片", "简历", "合同", "发票", "资料", "图片", "视频", "音乐", "下载", "桌面", "项目", "测试", "数据", "源码", "设计", "方案", "总结", "计划", "会议", "记录", "通知", "附件", "你好", "作业", "论文", "课件", "教程", "软件",
            "工作总结", "项目计划", "会议纪要", "财务报表", "产品说明", "用户手册", "你好世界", "毕业论文", "身份证明", "销售数据", "培训材料", "设计稿件", "合同附件", "测试报告", "采购清单", "系统设置", "网络配置",
            "张三的简历", "第一季度报告", "新建文件夹", "最终版方案", "客户资料汇总", "年度工作计划", "软件安装包", "数据库备份",
        ],
    },
    Sample {
        tag: "EUC-KR",
        encoding: NameEncoding::EucKr,
        locale: "ko-KR",
        names: &[
            "서울", "문서", "사진", "보고", "자료", "음악", "계약", "사업", "회의", "기록", "안녕", "학교", "과제", "논문", "강의", "자막", "영화", "음식", "여행", "가족",
            "회의록", "보고서", "계약서", "새폴더", "최종안", "사업계획", "졸업논문", "재무제표", "교육자료", "한국어파일", "제품설명서", "사용자매뉴얼", "신분증사본", "소프트웨어", "시스템설정",
            "홍길동이력서", "일분기보고서", "고객자료모음", "연간업무계획", "데이터베이스", "네트워크설정",
        ],
    },
    Sample {
        tag: "Big5",
        encoding: NameEncoding::Big5,
        locale: "zh-TW",
        names: &[
            "報告", "文件", "照片", "履歷", "合約", "發票", "資料", "圖片", "影片", "音樂", "下載", "桌面", "專案", "測試", "設計", "方案", "總結", "計畫", "會議", "紀錄",
            "工作總結", "專案計畫", "會議紀要", "財務報表", "產品說明", "使用手冊", "測試檔案", "畢業論文", "身分證明", "銷售資料", "教育訓練", "設計稿件",
            "王小明的履歷", "第一季報告", "新增資料夾", "最終版方案", "客戶資料彙整", "年度工作計畫",
        ],
    },
    Sample {
        tag: "Shift_JIS",
        encoding: NameEncoding::ShiftJis,
        locale: "ja-JP",
        names: &[
            "報告", "書類", "写真", "履歴", "契約", "資料", "画像", "動画", "音楽", "設計", "会議", "記録", "通知", "添付", "宿題", "論文", "授業", "旅行", "家族", "東京",
            "議事録", "業務報告", "事業計画", "財務諸表", "製品説明", "取扱説明書", "日本語ファイル", "卒業論文", "身分証明", "売上データ", "研修資料", "デザイン案",
            "山田太郎の履歴書", "第一四半期報告", "新しいフォルダー", "最終版の提案", "顧客資料まとめ", "年間業務計画",
        ],
    },
    Sample {
        tag: "CP1251",
        encoding: NameEncoding::Cp1251,
        locale: "ru-RU",
        names: &[
            "Фото", "Счёт", "Отчёт", "Музыка", "Договор", "Данные", "Проект", "Резюме", "Письмо", "Заметки", "Видео", "Книга", "Планы", "Работа", "Семья", "Привет", "Москва", "Задача", "Лекция", "Диплом",
            "Годовой отчёт", "План проекта", "Протокол собрания", "Финансовый отчёт", "Описание продукта", "Руководство пользователя", "Новая папка", "Итоговая версия", "Сводка по клиентам",
        ],
    },
];

/// 一组统计：总数、正确数、含糊数。
#[derive(Default, Debug)]
struct Stat {
    total: usize,
    correct: usize,
    ambiguous: usize,
    wrong: Vec<String>,
}

impl Stat {
    /// 正确率。
    fn accuracy(&self) -> f32 {
        self.correct as f32 / self.total.max(1) as f32
    }
}

/// 跑一组样本：`min_chars` 以上的名字计入；返回统计。
fn run(sample: &Sample, locale: Option<&str>, min_chars: usize, max_chars: usize) -> Stat {
    let mut stat = Stat::default();
    for name in sample.names {
        let chars = name.chars().filter(|c| !c.is_whitespace()).count();
        if chars < min_chars || chars > max_chars {
            continue;
        }
        let raw = enc(&format!("{name}.docx"), sample.encoding);
        let hints = Hints { made_by_dos: true, locale: locale.map(String::from) };
        let det = detect_raw(&[raw.as_slice()], &hints);
        stat.total += 1;
        if det.encoding == sample.encoding {
            stat.correct += 1;
        } else {
            stat.wrong.push(format!("{name}→{}{}", det.encoding.id(), if det.ambiguous { "?" } else { "!" }));
        }
        if det.ambiguous {
            stat.ambiguous += 1;
        }
    }
    stat
}

#[test]
fn three_plus_char_names_without_locale_are_mostly_right() {
    for s in SAMPLES {
        let stat = run(s, None, 3, usize::MAX);
        println!("{:9} 无 locale，≥3 字：{}/{} 正确，{} 含糊，错：{:?}", s.tag, stat.correct, stat.total, stat.ambiguous, stat.wrong);
        assert!(stat.accuracy() >= 0.9, "{} 无 locale 的 3 字以上名字正确率 {:.2} 低于 0.9：{:?}", s.tag, stat.accuracy(), stat.wrong);
    }
}

#[test]
fn names_with_own_locale_are_right_and_longer_ones_are_confident() {
    for s in SAMPLES {
        let stat = run(s, Some(s.locale), 2, usize::MAX);
        println!("{:9} 本地 locale，全部：{}/{} 正确，{} 含糊，错：{:?}", s.tag, stat.correct, stat.total, stat.ambiguous, stat.wrong);
        assert!(stat.accuracy() >= 0.97, "{} 本地 locale 下正确率 {:.2} 低于 0.97：{:?}", s.tag, stat.accuracy(), stat.wrong);
        let long = run(s, Some(s.locale), 4, usize::MAX);
        let confident = long.total - long.ambiguous;
        println!("{:9} 本地 locale，≥4 字：{}/{} 不含糊", s.tag, confident, long.total);
        assert!(confident as f32 / long.total as f32 >= 0.8, "{} 本地 locale 下 4 字以上名字有 {}/{} 含糊", s.tag, long.ambiguous, long.total);
    }
}

#[test]
fn wrong_locale_cannot_flip_longer_names() {
    // 用 zh-CN 先验去读其他语言：5 字以上仍应正确（谚文 ⊂ GB2312 一级字区的陷阱靠特异性 + chardetng 顶住）
    for s in SAMPLES.iter().filter(|s| s.tag != "GBK") {
        let stat = run(s, Some("zh-CN"), 5, usize::MAX);
        println!("{:9} zh-CN 先验，≥5 字：{}/{} 正确，错：{:?}", s.tag, stat.correct, stat.total, stat.wrong);
        assert!(stat.accuracy() >= 0.9, "{} 在 zh-CN 先验下 5 字以上名字正确率 {:.2}：{:?}", s.tag, stat.accuracy(), stat.wrong);
    }
}

#[test]
fn never_confidently_wrong_without_misleading_locale() {
    // 核心安全性质：没有 locale、或 locale 与内容匹配时，任何语言、任何长度，判错就必须标 ambiguous 让 UI 弹选择条。
    // 为什么不含错误 locale：先验的本意就是在字节证据打平时替本地用户做主（4 字韩文名与 15% 的 4 字中文名字节层面互相「完美」），
    // 这时跨语言的罕见包可能被自信地判错，用户用常驻的编码选择器改；代价是本地用户的常见包不用每次确认。
    let mut confident_wrong: Vec<String> = Vec::new();
    for s in SAMPLES {
        for locale in [None, Some("en-US"), Some(s.locale)] {
            let stat = run(s, locale, 2, usize::MAX);
            for w in &stat.wrong {
                if w.ends_with('!') {
                    confident_wrong.push(format!("{} locale={:?} {w}", s.tag, locale));
                }
            }
        }
    }
    assert!(confident_wrong.is_empty(), "自信地判错了：{confident_wrong:?}");
}

#[test]
fn two_char_names_report() {
    // 2 字名证据天然不足，这里只打印分布供调参参考，并要求本地 locale 下至少九成正确
    for s in SAMPLES {
        let none = run(s, None, 2, 2);
        let own = run(s, Some(s.locale), 2, 2);
        println!("{:9} 2 字：无 locale {}/{} 正确（{} 含糊）；本地 locale {}/{} 正确（{} 含糊）", s.tag, none.correct, none.total, none.ambiguous, own.correct, own.total, own.ambiguous);
        assert!(own.total == 0 || own.accuracy() >= 0.9, "{} 本地 locale 下 2 字名正确率 {:.2}：{:?}", s.tag, own.accuracy(), own.wrong);
    }
}
