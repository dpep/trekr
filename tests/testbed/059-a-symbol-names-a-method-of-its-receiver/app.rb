class Widget
  before_save :normalize
  alias_method :p, :percentile
  private :helper
  private_class_method :build

  def self.build
  end

  def percentile
    send(:helper)
    method(:normalize)
    respond_to?(:missing_thing)
  end

  def normalize
  end

  def helper
  end

  class << self
    alias_method :make, :build
  end
end

class Gadget
  def run(widget)
    widget.public_send(:percentile)
    Widget.new.send(:percentile)
    Widget.send(:build)
    [1].map(:percentile)
  end
end

class Record
  define_callbacks :save

  def save
  end
end
