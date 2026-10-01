class Base
  def self.rescue_from(*classes, with: nil); end
  def self.before_action(*names, if: nil, unless: nil, only: nil); end
  def self.validates(*names, **options); end
  def self.after_commit(*names, on: nil, if: nil); end
  def self.delegate(*names, to:); end
  def self.rate_limit!(name, response_method_name: nil); end
end

class WidgetsController < Base
  rescue_from StandardError, with: :respond_failed
  before_action :load_widget, if: :should_load?, only: [:show]
  validates :name, presence: true, unless: :skip_name?
  after_commit :publish, on: :create, if: [:publishable?, :ready?]
  delegate :token, to: :env_params
  rate_limit! :refresh, response_method_name: :render_throttled

  def show; end

  private

  def respond_failed; end
  def load_widget; end
  def should_load?; end
  def skip_name?; end
  def publish; end
  def publishable?; end
  def ready?; end
  def env_params; end
  def render_throttled; end
  def create; end
end
