class ApplicationController
  def self.before_action(*callbacks, **options, &block); end

  def authorize!; end
end
