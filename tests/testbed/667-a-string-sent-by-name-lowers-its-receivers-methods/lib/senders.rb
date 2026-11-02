class Lister
  def run(kind, user)
    Query.new(kind).public_send("list_#{kind}")
    Mailer.with(to: user).public_send("notify_#{kind}")
  end

  def total(record, kind)
    record.public_send("total_#{kind}")
  end
end

class Base
  include Rules

  def check(name)
    rule = "validate_#{name}"
    public_send(rule) if respond_to?(rule)
  end
end
