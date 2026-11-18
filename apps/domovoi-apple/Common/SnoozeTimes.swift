import Foundation

/// When a snooze can end, and how a wake time reads.
enum SnoozeTimes {
    struct Preset { let label: String; let ms: Int64 }

    static func presets(now: Date = .now, calendar: Calendar = .current) -> [Preset] {
        func at(_ day: Date, _ hour: Int) -> Date { calendar.date(bySettingHour: hour, minute: 0, second: 0, of: day) ?? day }
        let ms = { (d: Date) in Int64(d.timeIntervalSince1970 * 1000) }
        var out = [Preset(label: "In 1 Hour", ms: ms(now.addingTimeInterval(3600)))]
        let evening = at(now, 18)
        if evening.timeIntervalSince(now) > 3600 { out.append(Preset(label: "This Evening", ms: ms(evening))) }
        let tomorrow = calendar.date(byAdding: .day, value: 1, to: now) ?? now
        out.append(Preset(label: "Tomorrow Morning", ms: ms(at(tomorrow, 9))))
        if let monday = calendar.nextDate(after: now, matching: DateComponents(hour: 9, minute: 0, weekday: 2), matchingPolicy: .nextTime),
           monday.timeIntervalSince(at(tomorrow, 9)) > 3600 {
            out.append(Preset(label: "Next Week", ms: ms(monday)))
        }
        return out
    }

    /// "18:00", "Tue 09:00", or "12 Dec" for a wake time.
    static func label(_ ms: Int64, now: Date = .now, calendar: Calendar = .current) -> String {
        let date = Date(timeIntervalSince1970: Double(ms) / 1000)
        if calendar.isDate(date, inSameDayAs: now) { return date.formatted(date: .omitted, time: .shortened) }
        if date.timeIntervalSince(now) < 6 * 86_400 { return date.formatted(.dateTime.weekday(.abbreviated).hour().minute()) }
        return date.formatted(.dateTime.day().month(.abbreviated))
    }
}

/// How often a scheduled task can run: a short list rather than a stepper
/// that has to jump between minutes, hours and days.
enum ScheduleIntervals {
    static let all: [(minutes: Int, label: String)] = [
        (5, "5 minutes"), (15, "15 minutes"), (30, "30 minutes"), (60, "hour"), (180, "3 hours"),
        (360, "6 hours"), (720, "12 hours"), (1440, "day"), (10080, "week"),
    ]
}

/// A time from the server (ISO 8601), in this device's time zone.
enum ServerTime {
    static func date(_ iso: String) -> Date? {
        let parser = ISO8601DateFormatter()
        parser.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = parser.date(from: iso) { return date }
        parser.formatOptions = [.withInternetDateTime]
        return parser.date(from: iso)
    }

    /// "14:03", or with the date when it isn't today.
    static func local(_ iso: String) -> String {
        guard let date = date(iso) else { return iso }
        return date.formatted(date: Calendar.current.isDateInToday(date) ? .omitted : .abbreviated, time: .shortened)
    }
}
