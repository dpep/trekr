class Record
  def self.scope(name, body); end
end

class Widget < Record
  scope :ordered, -> { order(:position) }

  def self.recent
    ordered
  end
end

class Gadget < Record
  scope :ordered, -> { order(:name) }
end

class Connection
  attr_reader :url_prefix
  alias_method :prefix, :url_prefix

  def kind
    :ordered
  end

  def ordered?
    kind == :ordered
  end
end
