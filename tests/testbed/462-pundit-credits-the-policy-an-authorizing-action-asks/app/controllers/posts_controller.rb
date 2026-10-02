class PostsController < ApplicationController
  before_action :load_and_authorize, only: [:edit]

  def index
    authorize Post
  end

  def publish
    authorize @post
  end

  def create
    authorize Post.new
  end

  def edit
  end

  def update
  end

  def search
    @posts = policy_scope(Post)
  end

  def export
    @post
  end

  private

  def load_and_authorize
    @post = Post.find(1)
    authorize @post
  end
end
