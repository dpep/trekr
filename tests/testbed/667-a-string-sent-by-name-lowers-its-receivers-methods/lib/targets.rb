class Query
  def initialize(kind)
    @kind = kind
  end

  def list_open
    @kind
  end
end

class Other
  def list_closed
    :closed
  end
end

class Mailer
  def self.with(**)
    new
  end

  def notify_late
    :late
  end
end

module Rules
  def validate_size
    true
  end
end

class Totals
  def total_count
    0
  end
end
