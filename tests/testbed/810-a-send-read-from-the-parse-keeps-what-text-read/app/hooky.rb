module Hooky
  def run_hook = send(:hook)
end

class Host
  include Hooky
  def hook = 1
end
