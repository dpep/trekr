class WidgetError < StandardError
  def detail
  end
end

class Widget
  def run
    work
  rescue WidgetError => e
    e.detail
  end

  def retry_later
    work
  rescue => error
    error.message
  end

  def either
    work
  rescue WidgetError, ArgumentError => failure
    failure.detail
  end
end
