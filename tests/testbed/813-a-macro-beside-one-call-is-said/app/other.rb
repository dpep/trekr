class Other
  def self.before_action(*) = nil
  before_action :prepare
end

class Helper
  def go = prepare
  def prepare = 1
end
Helper.new.go
