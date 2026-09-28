class Record
  def readonly?
    false
  end

  def self.table_name
  end
end

class Summary < Record
  def readonly?
    true
  end

  def self.table_name
  end

  def tally
  end
end

module Frozen
  def readonly?
    true
  end
end

class Report < Record
  include Frozen
end

module Unmixed
  def readonly?
    true
  end
end
