class WidgetJob
  before_perform :prepare
  after_enqueue :log_enqueue
  around_perform :wrap

  def prepare
  end

  def log_enqueue
  end

  def wrap
    yield
  end

  def lonely
  end
end
